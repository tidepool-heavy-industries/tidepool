//! The single implicit session's manager: a thin policy wrapper over the
//! promoted `tidepool_runtime::session::registry::SingleSlot` — the ONE
//! session-ownership/lifecycle mechanism (see the root `CLAUDE.md` Mechanism
//! Index). The registry's [`Slot`](tidepool_runtime::session::registry::Slot)
//! (`Idle | Running | Suspended | Wedged`) is the ONLY lifecycle truth; this
//! module adds only what is genuinely REPL-specific policy on top of it:
//!
//! - a busy-guard that refuses a fresh `session_run` while `Suspended`
//!   ([`SessionManager::admit_run`]) — a policy choice the shared
//!   `checkout_run` deliberately does not make itself, since the harness's
//!   own keyed usage treats a run over parked frames as ordinary;
//! - the suspension's caller-facing payload (`captured` output, `expected_schema`,
//!   the reaper's TTL clock) — domain metadata the registry itself has no
//!   opinion on, mirroring how the harness keeps its own per-hole metadata
//!   OUTSIDE the registry (see `tidepool-harness/src/harness.rs`'s
//!   `pending_holes` map);
//! - the cancel-handle and live-bindings slots a turn/resource-read needs
//!   without checking the session out.

use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tidepool_codegen::jit_machine::CancelHandle;
use tidepool_mcp::CapturedOutput;
use tidepool_repr::SessionId;
use tidepool_runtime::session::registry::{CheckoutError, CheckoutReceipt, SingleSlot, SlotKind};
use tidepool_runtime::session::{admit_checkout, Aged};

use crate::session::Session;

/// An in-turn `ask` continuation id (`scont_<n>`). A minted-once identity,
/// not a free-form string — compared and routed as this newtype rather than
/// a bare `String`. `#[serde(transparent)]` keeps the wire form a plain
/// string, so `continuation_id` request/response JSON is byte-identical.
/// This is the registry's hole-identity type parameter for this crate —
/// `tidepool-harness` instantiates the SAME shared registry at its own
/// `HoleId` instead (see `tidepool-runtime::session::registry`'s module doc
/// for why the two newtypes stay separate).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct ContinuationId(pub String);

impl std::fmt::Display for ContinuationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A slot holding the resident machine's [`CancelHandle`], readable from the
/// async server side. `None` until the machine bootstraps on the session's
/// first expression turn. The [`Session`] publishes its handle here the
/// instant the machine bootstraps (via `set_cancel_slot`/`bootstrap_machine`)
/// and `reset()`s the flag at each turn start, so a timeout can `cancel()` an
/// in-flight runaway and the next turn starts clean.
pub type CancelSlot = Arc<Mutex<Option<CancelHandle>>>;

/// A fresh, empty cancel slot.
pub fn empty_cancel_slot() -> CancelSlot {
    Arc::new(Mutex::new(None))
}

/// A shared slot holding a JSON snapshot of the live session environment (the
/// decl plane + value/pure binds), republished after every completed turn.
/// The async server reads it directly for the `tidepool://session/bindings`
/// resource WITHOUT driving a turn — a lock-free-of-await read of live state.
pub type BindingsSlot = Arc<Mutex<serde_json::Value>>;

/// A fresh bindings slot seeded with the empty-session snapshot, so a
/// resource read before the first turn returns valid (empty) JSON rather
/// than null.
pub fn empty_bindings_slot() -> BindingsSlot {
    Arc::new(Mutex::new(serde_json::json!({
        "bindings": [],
        "generation": 0,
        "valGeneration": 0,
    })))
}

/// [`SessionManager`]'s type alias for the promoted `Checkout` at this
/// crate's machine (`Box<Session>`) and hole ([`ContinuationId`]) types.
pub type Checkout<'r> =
    tidepool_runtime::session::registry::Checkout<'r, Box<Session>, ContinuationId>;

/// This crate's instantiation of the promoted checkout error type.
pub type ManagerCheckoutError = CheckoutError<ContinuationId>;

/// The caller-facing payload of a pending `ask` suspension — everything
/// `session_resume` and the reaper need that the registry itself has no
/// opinion on. Lives OUTSIDE the registry, exactly one per manager (this
/// crate's session is single-hole: only one `ask` is ever pending at a
/// time), mirroring the harness's own per-hole metadata design.
struct SuspensionPayload {
    /// The pending continuation's id — kept here too (redundant with the
    /// registry's own hole set) purely so `session_resume` can build a
    /// "suspended on X, not Y" message without a separate registry read.
    cont_id: ContinuationId,
    /// The console output captured so far, carried across the suspension so
    /// the resumed turn's drain includes everything the pre-ask items
    /// printed.
    captured: CapturedOutput,
    /// The `ask`'s schema, used to validate + canonicalize the resume reply
    /// BEFORE the continuation is consumed. `None` ⇒ accept any JSON.
    expected_schema: Option<serde_json::Value>,
}

/// A pending suspension paired with its age via the kernel's abandonment-
/// liveness primitive (`tidepool_runtime::session::Aged`, #22 design doc
/// §3.2 item 5) — this crate's own TTL reaper (`server.rs`'s `reap_once`,
/// driven by [`SessionManager::suspension_since`]/[`SessionManager::
/// refresh_suspension_since`] below) is the SWEEP POLICY that reads
/// [`Aged::age`]; the kernel itself drives no timer and reclaims nothing on
/// its own (OQ3). This replaces a hand-rolled `since: Instant` field with
/// the shared primitive so a second consumer (`tidepool-harness`'s
/// `PendingSuspension`) can reuse the same "value + mint time + age query"
/// shape instead of re-deriving its own.
type Suspension = Aged<SuspensionPayload>;

/// The single implicit session's manager: holds AT MOST one resident
/// session, mirroring `tidepool_runtime::session::registry::SingleSlot`'s own
/// "no keying" shape (the multi-agent story is one repl server per agent).
pub struct SessionManager {
    slot: SingleSlot<Box<Session>, ContinuationId>,
    suspension: Mutex<Option<Suspension>>,
    cancel_slot: Mutex<Option<CancelSlot>>,
    bindings_slot: Mutex<Option<BindingsSlot>>,
}

impl Default for SessionManager {
    fn default() -> Self {
        SessionManager {
            slot: SingleSlot::new(),
            suspension: Mutex::new(None),
            cancel_slot: Mutex::new(None),
            bindings_slot: Mutex::new(None),
        }
    }
}

impl SessionManager {
    pub fn new() -> SessionManager {
        Self::default()
    }

    /// Install a freshly-opened session under `id`, seeded `Idle`. Errors
    /// (handing the session back) if one is already present — the caller
    /// lost an auto-open race and should drop this one and use the existing
    /// session. `id` is the caller's own session id (the same one its
    /// include-tree/session-config bookkeeping already minted) — the
    /// registry does not mint its own.
    pub fn install(&self, id: SessionId, mut session: Box<Session>) -> Result<(), Box<Session>> {
        let cancel_slot = empty_cancel_slot();
        // The session publishes its machine's cancel handle here at
        // bootstrap, so even a first-turn runaway is abortable.
        session.set_cancel_slot(cancel_slot.clone());
        self.slot.install(id, session)?;
        *self.cancel_slot.lock() = Some(cancel_slot);
        *self.bindings_slot.lock() = Some(empty_bindings_slot());
        Ok(())
    }

    /// Whether a session is currently installed at all.
    pub fn is_present(&self) -> bool {
        self.slot.current_id().is_some()
    }

    /// A busy-guard label for the current entry, or `None` if no session is
    /// installed — `run_command`'s "session is X; resume/reset first"
    /// rejection reads this. A thin read of the registry's own
    /// [`Slot::label`] (tidepool_runtime::session::registry::Slot::label).
    pub fn busy_label(&self) -> Option<String> {
        self.slot.label()
    }

    /// The current entry's [`SlotKind`], if one is installed — the reaper's
    /// dispatch on `Suspended` vs `Wedged`.
    pub fn slot_kind(&self) -> Option<SlotKind> {
        self.slot.kind()
    }

    /// The current entry's `Wedged` `since` timestamp — `None` unless it is
    /// actually `Wedged`.
    pub fn wedged_since(&self) -> Option<Instant> {
        self.slot.wedged_since()
    }

    /// Clone the shared cancel slot for the session, if present.
    pub fn cancel_slot(&self) -> Option<CancelSlot> {
        self.cancel_slot.lock().clone()
    }

    /// Clone the live bindings snapshot slot for the session, if present.
    pub fn bindings_slot(&self) -> Option<BindingsSlot> {
        self.bindings_slot.lock().clone()
    }

    /// The pending suspension's expected schema + a fresh clone of its
    /// captured-output buffer, if the session is suspended AND `cont_id`
    /// matches the pending one — what `session_resume` needs to validate a
    /// reply BEFORE consuming the continuation. Distinguishes the THREE
    /// resume-rejection causes `tidepool-repl/CLAUDE.md` documents: no
    /// suspension at all (`Err(None)`), suspended on a DIFFERENT
    /// continuation (`Err(Some(pending))`), or a match (`Ok((schema,
    /// captured))` — `schema` itself may be `None`, meaning "accept any
    /// JSON").
    pub fn suspension_for(
        &self,
        cont_id: &ContinuationId,
    ) -> Result<(Option<serde_json::Value>, CapturedOutput), Option<ContinuationId>> {
        match self.suspension.lock().as_ref() {
            None => Err(None),
            Some(s) if &s.get().cont_id == cont_id => {
                Ok((s.get().expected_schema.clone(), s.get().captured.clone()))
            }
            Some(s) => Err(Some(s.get().cont_id.clone())),
        }
    }

    /// Refresh the pending suspension's age clock (anti-starvation: a
    /// retrying continuation must not become the reaper's oldest-first
    /// eviction victim while its caller fixes an invalid reply). No-op if
    /// nothing is pending.
    pub fn refresh_suspension_since(&self) {
        if let Some(s) = self.suspension.lock().as_mut() {
            s.touch();
        }
    }

    /// The pending suspension's mint/refresh instant, for the reaper's TTL
    /// check. `None` if nothing is pending.
    pub fn suspension_since(&self) -> Option<Instant> {
        self.suspension.lock().as_ref().map(Aged::since)
    }

    /// The pending suspension's continuation id, for the reaper's abort
    /// path. `None` if nothing is pending.
    pub fn suspension_cont_id(&self) -> Option<ContinuationId> {
        self.suspension
            .lock()
            .as_ref()
            .map(|s| s.get().cont_id.clone())
    }

    /// Admit a NEW top-level run: `Idle → Running`. This is REPL POLICY, not
    /// the registry's own — the shared `checkout_run` allows a fresh run over
    /// a `Suspended` slot (the harness's multi-hole story), but this crate's
    /// documented contract is stricter: a session with a pending suspension
    /// accepts nothing but a resume or a reset. `None` when there is no
    /// session at all (the caller auto-opens first). `Some(Err(label))`
    /// carries the refused slot's own [`tidepool_runtime::session::registry::Slot::label`]
    /// — ready to drop into a rejection message — for EVERY refusal
    /// (`Running`, `Suspended`, `Wedged`) alike, one string instead of a
    /// per-variant match at the call site.
    ///
    /// Enforced by taking the checkout for real (the SAME atomic operation
    /// the registry itself uses — no separate check-then-checkout race) via
    /// the kernel's [`admit_checkout`] hook (#22 design doc §3.2 item 4):
    /// this crate supplies the "refuse anything non-empty" policy the
    /// kernel itself declines to have an opinion on, rather than
    /// hand-rolling the checkout-then-restore-if-refused dance inline.
    pub fn admit_run(&self) -> Option<Result<Checkout<'_>, String>> {
        self.slot.current_id()?;
        // Snapshot the label BEFORE checking out — `checkout_run` itself
        // mutates the slot to `Running`, so a label read AFTER it would
        // always say "running" regardless of what it was refused for.
        let label_before_checkout = self.slot.label().unwrap_or_default();
        Some(match self.slot.checkout_run() {
            Ok(co) => {
                admit_checkout(co, |holes| holes.is_empty()).map_err(|_holes| label_before_checkout)
            }
            Err(e) => Err(e.to_string()),
        })
    }

    /// `SessionRegistry::checkout_resume` against the current entry.
    pub fn checkout_resume(
        &self,
        cont_id: &ContinuationId,
    ) -> Result<Checkout<'_>, ManagerCheckoutError> {
        self.slot.checkout_resume(cont_id)
    }

    /// Record a fresh suspension's caller-facing payload — called once a
    /// turn's `TurnStep::Suspended` is observed and the checkout has already
    /// been restored `Suspended` at the registry level.
    fn set_suspension(
        &self,
        cont_id: ContinuationId,
        captured: CapturedOutput,
        expected_schema: Option<serde_json::Value>,
    ) {
        *self.suspension.lock() = Some(Aged::new(SuspensionPayload {
            cont_id,
            captured,
            expected_schema,
        }));
    }

    /// Clear the pending suspension (a resume consumed it, or a reset/removal
    /// dropped it).
    fn clear_suspension(&self) {
        *self.suspension.lock() = None;
    }

    /// Whether `id` is still the CURRENT entry — a settlement only touches
    /// the manager-level side state (`suspension`/`cancel_slot`/
    /// `bindings_slot`, all flat fields, unlike the registry's own
    /// per-entry epoch-guarded slot) when this is true. A settlement against
    /// a STALE id (the entry was replaced by a `session_reset` that raced
    /// it) must not mutate side state a FRESH, now-current session may
    /// already own — the registry's own `settle_*` calls stay safe on their
    /// own epoch guard regardless, but these flat fields have no epoch of
    /// their own, so this check is what keeps them from aliasing across a
    /// replace.
    fn is_current(&self, id: SessionId) -> bool {
        self.slot.current_id() == Some(id)
    }

    /// Settle a checkout as `Idle`, republishing the live bindings snapshot
    /// first (a decl/bind/reset may have changed the environment) and
    /// clearing any suspension — only when this checkout's session is still
    /// current (see [`Self::is_current`]).
    pub fn restore_idle(&self, receipt: CheckoutReceipt, session: Box<Session>) {
        if self.is_current(receipt.session_id()) {
            if let Some(slot) = self.bindings_slot() {
                *slot.lock() = session.bindings_snapshot();
            }
            self.clear_suspension();
        }
        self.slot.settle_suspended(receipt, session, Vec::new());
    }

    /// Settle a checkout as `Suspended{cont_id}`, recording the suspension's
    /// caller-facing payload — only when still current.
    pub fn restore_suspended(
        &self,
        receipt: CheckoutReceipt,
        session: Box<Session>,
        cont_id: ContinuationId,
        captured: CapturedOutput,
        expected_schema: Option<serde_json::Value>,
    ) {
        if self.is_current(receipt.session_id()) {
            self.set_suspension(cont_id.clone(), captured, expected_schema);
        }
        self.slot.settle_suspended(receipt, session, vec![cont_id]);
    }

    /// The turn never gave the session back (a runaway that outran its abort
    /// grace, or the blocking task itself crashed): settle the checkout as
    /// `Wedged{since}` — the registry's OWN persisted terminal state, visible
    /// to every future caller (not just the one holding this receipt) until
    /// the reaper/`session_reset` reclaims it. Clears any suspension (there
    /// is nothing left to resume) only when still current.
    pub fn mark_wedged(&self, receipt: CheckoutReceipt, since: Instant) {
        if self.is_current(receipt.session_id()) {
            self.clear_suspension();
        }
        self.slot.settle_wedged(receipt, since);
    }

    /// Settle a checkout by REMOVING the entry outright, with no reason
    /// worth keeping visible (an abort that unexpectedly re-suspended, with
    /// no caller left waiting on the fresh hole — the reaper's
    /// `abort_abandoned` path). Only when still current.
    pub fn retire(&self, receipt: CheckoutReceipt) {
        let current = self.is_current(receipt.session_id());
        self.slot.settle_retire(receipt);
        if current {
            self.clear_suspension();
            *self.cancel_slot.lock() = None;
            *self.bindings_slot.lock() = None;
        }
    }

    /// Remove the session wholesale (`session_reset`), dropping the resident
    /// machine — and with it any stowed `ask` continuation. Abort folds into
    /// reset. A turn still in flight will find its epoch stale on
    /// settlement.
    pub fn remove(&self) {
        self.slot.remove();
        self.clear_suspension();
        *self.cancel_slot.lock() = None;
        *self.bindings_slot.lock() = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tidepool_repr::SessionId;

    use crate::session::{BoxedStack, SessionConfig, DEFAULT_NURSERY_SIZE};
    use tidepool_mcp::EffectRoster;

    /// A real, openable `Session` tagged with `id`. `Session::open` only
    /// creates the session include dir and an empty decl log — no GHC, no
    /// machine (that boots lazily on the first real turn) — so the slot
    /// machine is testable with genuine sessions rather than a stand-in.
    fn test_session(id: u64, root: &std::path::Path) -> Box<Session> {
        let cfg = SessionConfig {
            id: SessionId(id),
            root: root.join(format!("session-{id}")),
            base_include: Vec::new(),
            roster: EffectRoster::from_handlers(&frunk::HNil),
            preamble: String::new(),
            effect_stack: String::new(),
            module_env: tidepool_runtime::session::ModuleEnv::standalone_default(),
            nursery_size: DEFAULT_NURSERY_SIZE,
        };
        let session = Session::open(cfg, Box::new(|| Box::new(frunk::HNil) as BoxedStack))
            .expect("a session opens on a fresh dir");
        Box::new(session)
    }

    #[test]
    fn empty_manager_has_no_entry() {
        let mgr = SessionManager::new();
        assert!(!mgr.is_present());
        assert!(mgr.cancel_slot().is_none());
        assert!(mgr.bindings_slot().is_none());
        assert!(mgr.admit_run().is_none());
        assert!(matches!(
            mgr.checkout_resume(&ContinuationId("scont_1".into())),
            Err(ManagerCheckoutError::NoSession)
        ));
    }

    /// `mark_wedged` leaves the registry's own terminal state behind — a
    /// SECOND caller (not just the one holding the receipt) sees "wedged",
    /// and the slot is freed only by an explicit `remove`. This is the fix
    /// the promotion's epoch/terminal-slot design makes possible: pre-
    /// promotion, the equivalent `retire()` call fully removed the entry, so
    /// only the ORIGINAL caller's own already-cloned `SharedState` ever
    /// showed "wedged" — a later caller's fresh read saw nothing and silently
    /// auto-opened. `busy_label()` now genuinely persists the reason.
    #[test]
    fn wedged_leaves_the_slot_visibly_wedged_until_removed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mgr = SessionManager::new();
        assert!(
            mgr.install(SessionId(1), test_session(1, dir.path()))
                .is_ok(),
            "install"
        );
        let checkout = mgr.admit_run().expect("idle -> run").expect("idle -> run");
        let (_session, receipt) = checkout.into_parts();

        mgr.mark_wedged(receipt, Instant::now());
        assert!(
            mgr.busy_label().is_some_and(|l| l.contains("wedged")),
            "a wedged entry must stay visible as wedged, not vanish"
        );
        assert!(
            mgr.admit_run().unwrap().is_err(),
            "wedged refuses a fresh run"
        );

        mgr.remove();
        assert!(!mgr.is_present(), "remove reclaims a wedged entry");
        assert!(
            mgr.install(SessionId(2), test_session(2, dir.path()))
                .is_ok(),
            "the freed slot accepts a fresh session"
        );
    }

    /// THE EPOCH GUARD, driven through this crate's own facade — a stale
    /// turn from BEFORE a `session_reset` must not clobber the session
    /// installed after it.
    #[test]
    fn a_stale_turn_cannot_clobber_a_session_installed_after_a_reset() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mgr = SessionManager::new();

        assert!(
            mgr.install(SessionId(1), test_session(1, dir.path()))
                .is_ok(),
            "install"
        );
        let stale = mgr.admit_run().expect("idle -> run").expect("idle -> run");
        let (stale_session, stale_receipt) = stale.into_parts();

        mgr.remove();
        assert!(
            mgr.install(SessionId(2), test_session(2, dir.path()))
                .is_ok(),
            "a fresh session installs after the reset"
        );

        mgr.restore_idle(stale_receipt, stale_session);
        let fresh = mgr
            .admit_run()
            .expect("the fresh session is still Idle and checkoutable")
            .expect("idle -> run");
        assert_eq!(
            fresh.session_id(),
            SessionId(2),
            "a stale restore must not clobber the fresh entry"
        );
        let (session, receipt) = fresh.into_parts();
        mgr.restore_idle(receipt, session);
    }
}
