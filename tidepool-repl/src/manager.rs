//! The single implicit session's manager: an ownership slot machine around ONE
//! resident [`Session`].
//!
//! There is no worker thread. A turn CHECKS the session OUT of the slot (leaving
//! it [`SessionSlot::Running`]), runs it on the blocking pool with the manager
//! lock RELEASED, and restores it as `Idle` or `Suspended{cont_id}` under a
//! second short lock. That is the harness's `SessionRegistry` discipline
//! (`tidepool-harness/src/registry.rs`) at N = 1 — which is itself a port of this
//! crate's `state.rs` discipline, so this closes the circle.
//!
//! The slot is the OWNERSHIP truth (where the session is); [`SessionState`] is
//! the caller-facing LIFECYCLE truth (what the server tells a client and which
//! ops it admits, including `Wedged`/`Closing`, which have no slot of their own).
//! The two are transitioned together at the dispatch boundary.
//!
//! Every checkout carries an EPOCH. `session_reset` replaces the whole entry, so
//! an in-flight turn can outlive the entry it was checked out of; restoring
//! against a stale epoch DROPS the session instead of clobbering the fresh one.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use tidepool_codegen::jit_machine::CancelHandle;

use crate::session::Session;
use crate::state::{shared, ContinuationId, SessionState, SharedState};

/// A slot holding the resident machine's [`CancelHandle`], readable from the
/// async server side. `None` until the machine bootstraps on the session's first
/// expression turn. The [`Session`] publishes its handle here the instant the
/// machine bootstraps (via `set_cancel_slot`/`bootstrap_machine`) and `reset()`s
/// the flag at each turn start, so a timeout can `cancel()` an in-flight runaway
/// and the next turn starts clean.
pub type CancelSlot = Arc<Mutex<Option<CancelHandle>>>;

/// A fresh, empty cancel slot.
pub fn empty_cancel_slot() -> CancelSlot {
    Arc::new(Mutex::new(None))
}

/// A shared slot holding a JSON snapshot of the live session environment (the
/// decl plane + value/pure binds), republished after every completed turn. The
/// async server reads it directly for the `tidepool://session/bindings` resource
/// WITHOUT driving a turn — a lock-free-of-await read of live state.
pub type BindingsSlot = Arc<Mutex<serde_json::Value>>;

/// A fresh bindings slot seeded with the empty-session snapshot, so a resource
/// read before the first turn returns valid (empty) JSON rather than null.
pub fn empty_bindings_slot() -> BindingsSlot {
    Arc::new(Mutex::new(serde_json::json!({
        "bindings": [],
        "generation": 0,
        "valGeneration": 0,
    })))
}

/// Where the resident session is right now. The `unsafe impl Send` on the
/// machine and the binding table is justified by exactly this: the session is in
/// EXACTLY one place — a slot variant, or the turn that checked it out.
enum SessionSlot {
    /// Present and ready for a new turn.
    Idle(Box<Session>),
    /// Checked out: a turn owns it on the blocking pool.
    Running,
    /// Present, holding a stowed `ask` continuation. Accepts only a resume of
    /// `cont_id` (or a wholesale [`SessionManager::remove`]).
    Suspended {
        session: Box<Session>,
        cont_id: ContinuationId,
    },
}

/// One manager entry: the session slot plus everything that outlives an
/// individual turn. State lives here (not smeared across the server's maps) so
/// it is owned in one place and transitioned atomically — see [`crate::state`].
struct SessionEntry {
    /// Identity of THIS entry. A checkout records it; a restore against a
    /// different epoch means the entry was replaced (reset) mid-turn.
    epoch: u64,
    slot: SessionSlot,
    state: SharedState,
    cancel_slot: CancelSlot,
    bindings_slot: BindingsSlot,
}

/// A session checked OUT of the manager for the duration of one turn. The turn
/// owns the session; it must hand it back through
/// [`SessionManager::restore_idle`] / [`SessionManager::restore_suspended`], or
/// declare it lost through [`SessionManager::drop_entry`] — all three keyed on
/// [`Self::epoch`].
pub struct Checkout {
    /// The session, moved out of its slot.
    pub session: Box<Session>,
    /// The entry epoch this checkout came from.
    pub epoch: u64,
}

/// The single implicit session's manager: holds AT MOST one resident session.
/// The multi-agent story is one repl server per agent, so there is exactly one
/// current session — no keying. `session_run` auto-installs it on first use;
/// `session_reset` swaps in a fresh one.
#[derive(Default)]
pub struct SessionManager {
    entry: Mutex<Option<SessionEntry>>,
    next_epoch: AtomicU64,
}

impl SessionManager {
    pub fn new() -> SessionManager {
        SessionManager {
            entry: Mutex::new(None),
            next_epoch: AtomicU64::new(1),
        }
    }

    /// Install a freshly-opened session, seeded `Idle`. Errors (handing the
    /// session back) if one is already present — the caller lost an auto-open
    /// race and should drop this one and use the existing session.
    pub fn install(&self, mut session: Box<Session>) -> Result<(), Box<Session>> {
        let mut slot = self.entry.lock();
        if slot.is_some() {
            return Err(session);
        }
        let cancel_slot = empty_cancel_slot();
        // The session publishes its machine's cancel handle here at bootstrap,
        // so even a first-turn runaway is abortable.
        session.set_cancel_slot(cancel_slot.clone());
        *slot = Some(SessionEntry {
            epoch: self.next_epoch.fetch_add(1, Ordering::Relaxed),
            slot: SessionSlot::Idle(session),
            state: shared(SessionState::Idle),
            cancel_slot,
            bindings_slot: empty_bindings_slot(),
        });
        Ok(())
    }

    /// Clone the shared lifecycle state for the session, if present. The server
    /// locks this (briefly, never across an `.await`) to read/drive transitions.
    pub fn state(&self) -> Option<SharedState> {
        self.entry.lock().as_ref().map(|e| e.state.clone())
    }

    /// Clone the shared cancel slot for the session, if present. The server
    /// reads the resident machine's [`CancelHandle`] from it on timeout to abort
    /// a runaway turn at a JIT safepoint.
    pub fn cancel_slot(&self) -> Option<CancelSlot> {
        self.entry.lock().as_ref().map(|e| e.cancel_slot.clone())
    }

    /// Clone the live bindings snapshot slot for the session, if present. The
    /// server reads it for the `tidepool://session/bindings` resource.
    pub fn bindings_slot(&self) -> Option<BindingsSlot> {
        self.entry.lock().as_ref().map(|e| e.bindings_slot.clone())
    }

    /// Check the session OUT for a new turn: `Idle → Running`. `None` when there
    /// is no session, or it is already running, or it is suspended — the server's
    /// [`SessionState`] busy-guard has already rejected those cases, so a `None`
    /// here is a lost race, not the normal refusal path.
    pub fn checkout_run(&self) -> Option<Checkout> {
        let mut guard = self.entry.lock();
        let entry = guard.as_mut()?;
        match std::mem::replace(&mut entry.slot, SessionSlot::Running) {
            SessionSlot::Idle(session) => Some(Checkout {
                session,
                epoch: entry.epoch,
            }),
            other => {
                entry.slot = other;
                None
            }
        }
    }

    /// Check the session OUT to resume its pending continuation:
    /// `Suspended{cont_id} → Running`, validating `cont_id` matches. A mismatch
    /// leaves the slot untouched — validate-before-consume, so a wrong id can't
    /// spend the pending hole.
    pub fn checkout_resume(&self, cont_id: &ContinuationId) -> Option<Checkout> {
        let mut guard = self.entry.lock();
        let entry = guard.as_mut()?;
        let matches =
            matches!(&entry.slot, SessionSlot::Suspended { cont_id: p, .. } if p == cont_id);
        if !matches {
            return None;
        }
        match std::mem::replace(&mut entry.slot, SessionSlot::Running) {
            SessionSlot::Suspended { session, .. } => Some(Checkout {
                session,
                epoch: entry.epoch,
            }),
            other => {
                entry.slot = other;
                None
            }
        }
    }

    /// Restore a checked-out session as `Idle` (the turn completed).
    /// A stale `epoch` (the entry was replaced by `session_reset` mid-turn)
    /// DROPS the session here rather than resurrecting it over the fresh one.
    pub fn restore_idle(&self, epoch: u64, session: Box<Session>) {
        let mut guard = self.entry.lock();
        match guard.as_mut() {
            Some(entry) if entry.epoch == epoch => entry.slot = SessionSlot::Idle(session),
            _ => drop(session),
        }
    }

    /// Restore a checked-out session as `Suspended{cont_id}` (the turn stowed an
    /// `ask` continuation). Same stale-epoch rule as [`Self::restore_idle`].
    pub fn restore_suspended(&self, epoch: u64, session: Box<Session>, cont_id: ContinuationId) {
        let mut guard = self.entry.lock();
        match guard.as_mut() {
            Some(entry) if entry.epoch == epoch => {
                entry.slot = SessionSlot::Suspended { session, cont_id }
            }
            _ => drop(session),
        }
    }

    /// The turn never gave the session back (a runaway that outran its abort
    /// grace): drop the WHOLE entry. Honest bookkeeping — the session was moved
    /// into the blocking closure, so there is nothing left to restore and a slot
    /// claiming otherwise would lie. The next `session_run` auto-opens a fresh
    /// session; `session_reset` does the same explicitly. No-op on a stale epoch.
    pub fn drop_entry(&self, epoch: u64) {
        let mut guard = self.entry.lock();
        if guard.as_ref().is_some_and(|e| e.epoch == epoch) {
            *guard = None;
        }
    }

    /// Remove the session wholesale (`session_reset`), dropping the resident
    /// machine — and with it any stowed `ask` continuation. Abort folds into
    /// reset. A turn still in flight will find its epoch stale on restore.
    pub fn remove(&self) {
        *self.entry.lock() = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tidepool_repr::SessionId;

    use crate::session::{BoxedStack, SessionConfig, DEFAULT_NURSERY_SIZE};

    /// A real, openable `Session` tagged with `id`. `Session::open` only creates
    /// the session include dir and an empty decl log — no GHC, no machine (that
    /// boots lazily on the first real turn) — so the slot machine is testable
    /// with genuine sessions rather than a stand-in, and `Session::id` gives the
    /// tests a way to say WHICH session is in the slot.
    fn test_session(id: u64, root: &std::path::Path) -> Box<Session> {
        let cfg = SessionConfig {
            id: SessionId(id),
            root: root.join(format!("session-{id}")),
            base_include: Vec::new(),
            decls: Vec::new(),
            preamble: String::new(),
            effect_stack: String::new(),
            ask_tag: 0,
            module_env: tidepool_runtime::session::ModuleEnv::standalone_default(),
            nursery_size: DEFAULT_NURSERY_SIZE,
        };
        let session = Session::open(cfg, Box::new(|| Box::new(frunk::HNil) as BoxedStack))
            .expect("a session opens on a fresh dir");
        Box::new(session)
    }

    /// The slot transitions on an absent entry: every operation is inert rather
    /// than a panic. The full checkout/restore round trip on a live machine is
    /// covered end-to-end by the suspension suites (`tests/ask_resume.rs`,
    /// `tests/lifecycle_state.rs`), which drive real turns through
    /// `dispatch_tool`.
    #[test]
    fn empty_manager_has_no_entry() {
        let mgr = SessionManager::new();
        assert!(mgr.state().is_none());
        assert!(mgr.cancel_slot().is_none());
        assert!(mgr.bindings_slot().is_none());
        assert!(mgr.checkout_run().is_none());
        assert!(mgr
            .checkout_resume(&ContinuationId("scont_1".into()))
            .is_none());
        // Restoring/dropping against an absent entry is inert, not a panic.
        mgr.drop_entry(1);
        mgr.remove();
    }

    /// The POSITIVE half of the wedge path: `drop_entry` with the CURRENT epoch
    /// really does retire the entry, so a wedged turn's session slot is freed
    /// rather than left `Running` forever. (The stale-epoch half — the same call
    /// arriving after a reset — is the ABA test below.)
    #[test]
    fn drop_entry_on_the_current_epoch_retires_the_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mgr = SessionManager::new();
        assert!(mgr.install(test_session(1, dir.path())).is_ok(), "install");
        let checkout = mgr.checkout_run().expect("idle → running");

        mgr.drop_entry(checkout.epoch);
        assert!(
            mgr.state().is_none(),
            "a wedged turn's entry must be retired, not left Running"
        );
        // The slot is free again: a fresh session installs.
        assert!(
            mgr.install(test_session(2, dir.path())).is_ok(),
            "the freed slot accepts a fresh session"
        );
    }

    /// THE EPOCH GUARD — an ABA on the session slot.
    ///
    /// A turn owns its session out on the blocking pool while `session_reset`
    /// can remove the entry and install a FRESH session underneath it, and the
    /// epoch is the ONLY thing distinguishing "hand back / retire the entry I
    /// was checked out of" from "…whatever is there now".
    ///
    /// The interleaving, driven here at the manager level (no GHC needed): a
    /// turn checks out, a reset swaps the entry, and only THEN does the stale
    /// turn take each of its three exits. All three must be inert, and the
    /// fresh session must still be the one in the slot — asserted by session
    /// id, because a missing guard would silently leave the STALE session
    /// installed and drop the fresh one.
    #[test]
    fn a_stale_turn_cannot_clobber_a_session_installed_after_a_reset() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mgr = SessionManager::new();

        // --- exit 1: `drop_entry` (the two wedge paths) ----------------------
        assert!(mgr.install(test_session(1, dir.path())).is_ok(), "install");
        let stale = mgr.checkout_run().expect("idle → running");

        // `session_reset` lands mid-turn: entry removed, fresh session installed.
        mgr.remove();
        assert!(
            mgr.install(test_session(2, dir.path())).is_ok(),
            "a fresh session installs after the reset"
        );
        let fresh_state = mgr.state().expect("fresh entry present");

        // The stale turn now declares itself wedged. It must NOT take the fresh
        // entry with it.
        mgr.drop_entry(stale.epoch);
        let after_wedge = mgr
            .state()
            .expect("a stale wedge must not remove the entry installed after the reset");
        assert!(
            Arc::ptr_eq(&after_wedge, &fresh_state),
            "the fresh entry must be untouched, not replaced"
        );

        // --- exit 2: `restore_idle` (the completed-turn path) ---------------
        let Checkout {
            session: stale_session,
            epoch: stale_epoch,
        } = stale;
        mgr.restore_idle(stale_epoch, stale_session);
        let fresh = mgr
            .checkout_run()
            .expect("the fresh session is still Idle and checkoutable");
        assert_eq!(
            fresh.session.id(),
            SessionId(2),
            "a stale restore must drop its session, not install it over the fresh one"
        );

        // --- exit 3: `restore_suspended` (the stowed-ask path) --------------
        // The same interleaving again, now with session 2 as the stale turn.
        let Checkout {
            session: stale2,
            epoch: stale2_epoch,
        } = fresh;
        mgr.remove();
        assert!(
            mgr.install(test_session(3, dir.path())).is_ok(),
            "a third session installs"
        );
        mgr.restore_suspended(stale2_epoch, stale2, ContinuationId("scont_stale".into()));
        // `checkout_run` refuses a Suspended slot, so its success is itself the
        // proof that the stale hole was not installed on the fresh entry.
        let third = mgr
            .checkout_run()
            .expect("the third session is still Idle — not Suspended on a stale hole");
        assert_eq!(
            third.session.id(),
            SessionId(3),
            "a stale suspend-restore must drop its session, not install it over the fresh one"
        );
    }
}
