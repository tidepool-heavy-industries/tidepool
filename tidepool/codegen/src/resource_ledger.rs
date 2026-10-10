//! Runtime-resource ownership for one JIT machine.
//!
//! This module owns identities and scope membership for parked continuations,
//! live value handles, and scope-local cancellation flags. It deliberately
//! owns each retained cell and its collector registration. A frame-owned
//! managed-root value moves the original cell with its exact representation.
//! Scope closure removes entries before their cell owners settle registration.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::old_space::{OwnedRootCell, RootSlot};
use crate::prepared_program::ProgramId;
use crate::suspension::{ContinuationId, RealmId, ValueHandle};
use tidepool_repr::execution_schema::{RuntimeRep, TypeNodeId, ValueId};

/// What a resume needs to interpret the answer and re-enter a prepared
/// continuation. The evidence owner and the runner can be different
/// programs: a retained closure from an earlier turn may reach a site that
/// turn declared, while the turn that invoked it owns the resume entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreparedFrameEvidence {
    pub reply: PreparedReplyEvidence,
    /// The program whose admitted resume entry re-enters the continuation.
    pub runner: ProgramId,
    /// `runner`'s `__resume` top: `\q x -> settle (resumeLifted q x)`.
    pub resume_entry: ValueId,
    /// The representation the continuation was retained with.
    pub continuation_rep: RuntimeRep,
}

/// Immutable evidence retained independently of the runner's resume entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreparedReplyEvidence {
    Static {
        owner: ProgramId,
        constructor: tidepool_repr::DataConId,
        node: TypeNodeId,
    },
    StaticWithSite {
        owner: ProgramId,
        constructor: tidepool_repr::DataConId,
        node: TypeNodeId,
        site_owner: ProgramId,
        site_row: usize,
        payload_field: u32,
        capture_input: Option<u32>,
    },
    AtSite {
        owner: ProgramId,
        row: usize,
    },
}

impl PreparedReplyEvidence {
    pub fn site_owner(self) -> Option<ProgramId> {
        match self {
            Self::StaticWithSite { site_owner, .. } => Some(site_owner),
            _ => None,
        }
    }

    pub fn owner(self) -> ProgramId {
        match self {
            Self::Static { owner, .. } | Self::StaticWithSite { owner, .. }
                | Self::AtSite { owner, .. } => owner,
        }
    }
}

pub(crate) type FrameEvidence = PreparedFrameEvidence;

/// Atomic frame ownership of a persistent cell and its exact representation.
/// Only the cell owns registration settlement and physical storage.
pub(crate) struct OwnedManagedRoot {
    cell: OwnedRootCell,
    rep: RuntimeRep,
}

impl OwnedManagedRoot {
    pub(crate) fn addr(&self) -> *mut *mut u8 {
        self.cell.addr()
    }

    pub(crate) fn into_parts(self) -> (OwnedRootCell, RuntimeRep) {
        (self.cell, self.rep)
    }
}

/// One parked continuation and all policy needed to resume it.
pub(crate) struct ContinuationFrame {
    pub(crate) cell: OwnedRootCell,
    pub(crate) realm: RealmId,
    pub(crate) live_payload_root: Option<OwnedManagedRoot>,
    pub(crate) evidence: FrameEvidence,
}

/// One live handle's rooted slot, cleanup owner, and the representation of
/// the word its slot holds. Prepared STG mints only lifted heap pointers; the
/// prepared machine also mints `UnliftedRef` handles (byte arrays, boxed
/// arrays), and a bare-handle delivery must recover that representation from
/// the ledger, not assume it.
pub(crate) struct HandleEntry {
    pub(crate) slot: OwnedRootCell,
    pub(crate) realm: RealmId,
    pub(crate) rep: RuntimeRep,
    pub(crate) class: HandleClass,
}

/// What a rooted handle is accounted as. Both classes share one handle
/// namespace -- an import slot is published from either without knowing
/// which -- but they are counted apart, so a value-handle leak cannot hide
/// behind a machine's growing export set and a runaway export set cannot
/// hide behind a turn's handles.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HandleClass {
    /// A turn's own retained value: bound, delivered, parked or observed.
    /// Released when its owner releases it, or when its resource scope closes.
    Value,
    /// An installed program's top, retained for the machine's lifetime so
    /// later programs can import it instead of compiling their own copy.
    /// Owned independently of resource-scope lifetimes and never taken by scope closure.
    CodeExport,
}

/// The one machine-local owner for opaque rooted-value identities.
///
/// Continuation policy composes this ledger; prepared execution uses the same
/// ledger directly with `RealmId::ROOT`.  Neither path mints a second handle
/// namespace or exposes a raw root slot to its caller.
#[derive(Default)]
pub(crate) struct RootHandleLedger {
    handles: HashMap<u64, HandleEntry>,
    /// Live [`HandleClass::CodeExport`] entries in `handles`, maintained by
    /// every insert and removal. A machine carries thousands of exports and
    /// only a handful of value handles, and the counts are read on the
    /// rooting receipt at every park and close; neither class is counted by
    /// walking the map.
    code_exports: usize,
}

impl RootHandleLedger {
    pub(crate) fn try_reserve(
        &mut self,
        additional: usize,
    ) -> Result<(), std::collections::TryReserveError> {
        self.handles.try_reserve(additional)
    }

    /// Live [`HandleClass::Value`] handles.
    pub(crate) fn len(&self) -> usize {
        self.handles.len() - self.code_exports
    }

    /// Live [`HandleClass::CodeExport`] handles.
    pub(crate) fn code_exports(&self) -> usize {
        self.code_exports
    }

    pub(crate) fn insert(
        &mut self,
        slot: OwnedRootCell,
        realm: RealmId,
        rep: RuntimeRep,
        class: HandleClass,
    ) -> ValueHandle {
        let handle = ValueHandle::fresh();
        let replaced = self.handles.insert(
            handle.0,
            HandleEntry {
                slot,
                realm,
                rep,
                class,
            },
        );
        debug_assert!(replaced.is_none(), "fresh value handle must not collide");
        if class == HandleClass::CodeExport {
            self.code_exports += 1;
        }
        handle
    }

    pub(crate) fn get(&self, handle: ValueHandle) -> Option<&HandleEntry> {
        self.handles.get(&handle.0)
    }

    pub(crate) fn take(&mut self, handle: ValueHandle) -> Option<HandleEntry> {
        let entry = self.handles.remove(&handle.0)?;
        if entry.class == HandleClass::CodeExport {
            self.code_exports -= 1;
        }
        Some(entry)
    }

    pub(crate) fn rehome(&mut self, handle: ValueHandle, realm: RealmId) -> bool {
        let Some(entry) = self.handles.get_mut(&handle.0) else {
            return false;
        };
        entry.realm = realm;
        true
    }

    /// Every live handle's root slot.
    pub(crate) fn slots(&self) -> impl Iterator<Item = RootSlot> + '_ {
        self.handles.values().map(|entry| entry.slot.physical())
    }

    /// Every [`HandleClass::Value`] handle owned by `realm`. A code export
    /// outlives every resource scope -- it is the machine's own root on installed
    /// code, not a turn's lease -- so scope closure never takes one, and the
    /// export count this removal path never touches stays correct.
    pub(crate) fn take_realm(&mut self, realm: RealmId) -> Vec<HandleEntry> {
        let ids: Vec<_> = self
            .handles
            .iter()
            .filter_map(|(&id, entry)| {
                (entry.realm == realm && entry.class == HandleClass::Value).then_some(id)
            })
            .collect();
        ids.into_iter()
            .filter_map(|id| self.handles.remove(&id))
            .collect()
    }
}

/// Entries removed by one scope closure.
pub(crate) struct ClosedRealm {
    pub(crate) frames: Vec<ContinuationFrame>,
    pub(crate) handles: Vec<HandleEntry>,
}

/// Read-only resource counts at a machine quiescence point.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResourceCounts {
    pub parked_continuations: usize,
    pub value_handles: usize,
    /// Installed-program tops retained for later programs to import
    /// ([`HandleClass::CodeExport`]). Counted apart from `value_handles` so
    /// neither class can mask the other's growth.
    pub code_exports: usize,
    pub cancellation_scopes: usize,
}

/// External handle for cancelling a running machine — either engine's:
/// `PreparedMachine::cancel_handle`/`realm_cancel_handle` and
/// `PreparedMachine::realm_cancel_handle` both wrap one of this module's own
/// `cancel_flag` entries in this same handle type rather than each defining
/// their own.
///
/// `CancelHandle` is `Send + Sync + Clone`, so callers can hand clones to
/// watchdog threads. In prepared execution, cancellation is observed at the next GC
/// safepoint (heap check), which fires on essentially every non-trivial
/// allocation in Haskell code, and the running program unwinds via the
/// normal cancellation error path. On
/// prepared, it's observed at the next `PreparedSafepoint`
/// (`Allocation`/`FunctionEntry`/`Backedge`/`ThunkEntry`/`ThunkCommit`).
///
/// The flag is per-scope (per-`PreparedMachine`, or per-`RealmId` on the
/// prepared route), not per-run: call [`Self::reset`] between runs if you
/// intend to reuse the machine/resource scope after a cancellation.
#[derive(Clone, Debug)]
pub struct CancelHandle(Arc<AtomicBool>);

impl CancelHandle {
    /// Wrap an existing flag as a `CancelHandle`.
    pub(crate) fn from_flag(flag: Arc<AtomicBool>) -> Self {
        Self(flag)
    }

    /// Request cancellation of the associated machine/scope. The running
    /// program (if any) will abort at its next safepoint.
    pub fn cancel(&self) {
        // SeqCst is overkill for correctness here (the running thread's
        // relaxed load will observe the store eventually), but this is not a
        // hot path — it is called once from a watchdog — so we prefer the
        // stronger ordering for debuggability.
        self.0.store(true, Ordering::SeqCst);
    }

    /// Returns `true` if cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// Clear a previous cancellation request. Call this between runs if the
    /// same machine/scope is reused after a cancelled run.
    pub fn reset(&self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// The process-local ownership ledger for one JIT machine.
#[derive(Default)]
pub(crate) struct ResourceLedger {
    continuations: HashMap<ContinuationId, ContinuationFrame>,
    next_continuation_id: u64,
    handles: RootHandleLedger,
    cancel_flags: HashMap<RealmId, Arc<AtomicBool>>,
    #[cfg(test)]
    pub(crate) fail_next_handle_reservation: bool,
}

impl ResourceLedger {
    pub(crate) fn counts(&self) -> ResourceCounts {
        ResourceCounts {
            parked_continuations: self.continuations.len(),
            value_handles: self.handles.len(),
            code_exports: self.handles.code_exports(),
            cancellation_scopes: self.cancel_flags.len(),
        }
    }

    pub(crate) fn cancel_flag(&mut self, realm: RealmId) -> Arc<AtomicBool> {
        self.cancel_flags
            .entry(realm)
            .or_insert_with(|| Arc::new(AtomicBool::new(false)))
            .clone()
    }

    pub(crate) fn try_reserve_continuations(
        &mut self,
        count: usize,
    ) -> Result<(), std::collections::TryReserveError> {
        self.continuations.try_reserve(count)
    }

    pub(crate) fn park(&mut self, frame: ContinuationFrame) -> ContinuationId {
        let id = ContinuationId(self.next_continuation_id);
        self.next_continuation_id += 1;
        let replaced = self.continuations.insert(id, frame);
        debug_assert!(replaced.is_none(), "fresh continuation id must not collide");
        id
    }

    pub(crate) fn continuation(&self, id: ContinuationId) -> Option<&ContinuationFrame> {
        self.continuations.get(&id)
    }

    pub(crate) fn continuation_mut(
        &mut self,
        id: ContinuationId,
    ) -> Option<&mut ContinuationFrame> {
        self.continuations.get_mut(&id)
    }

    pub(crate) fn take_continuation(&mut self, id: ContinuationId) -> Option<ContinuationFrame> {
        self.continuations.remove(&id)
    }

    /// The root slot of every live handle: what a machine-level mark seeds
    /// from, beside the parked frames' cells and payload roots.
    pub(crate) fn handle_slots(&self) -> impl Iterator<Item = RootSlot> + '_ {
        self.handles.slots()
    }

    /// Every parked frame's continuation cell and untaken live-payload root,
    /// plus the frame's prepared evidence when it has one.
    pub(crate) fn frame_roots(&self) -> Vec<(*mut *mut u8, Option<&PreparedFrameEvidence>)> {
        self.continuations
            .values()
            .flat_map(|frame| {
                let evidence = Some(&frame.evidence);
                let payload = frame
                    .live_payload_root
                    .as_ref()
                    .map(|root| (root.addr(), evidence));
                std::iter::once((frame.cell.addr(), evidence)).chain(payload)
            })
            .collect()
    }

    pub(crate) fn parked_ids(&self) -> Vec<ContinuationId> {
        let mut ids: Vec<_> = self.continuations.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    pub(crate) fn try_reserve_handles(
        &mut self,
        additional: usize,
    ) -> Result<(), std::collections::TryReserveError> {
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next_handle_reservation) {
            return Vec::<u8>::new().try_reserve(usize::MAX);
        }
        self.handles.try_reserve(additional)
    }

    /// Mint a handle for `slot`, recording `rep` as the representation of the
    /// word the slot holds (see [`HandleEntry::rep`]).
    pub(crate) fn insert_handle(
        &mut self,
        slot: OwnedRootCell,
        realm: RealmId,
        rep: RuntimeRep,
    ) -> ValueHandle {
        self.handles.insert(slot, realm, rep, HandleClass::Value)
    }

    /// [`Self::insert_handle`] for a machine-lifetime export root: same
    /// handle namespace, counted as [`HandleClass::CodeExport`].
    pub(crate) fn insert_export_handle(
        &mut self,
        slot: OwnedRootCell,
        rep: RuntimeRep,
    ) -> ValueHandle {
        self.handles
            .insert(slot, RealmId::ROOT, rep, HandleClass::CodeExport)
    }

    pub(crate) fn handle(&self, handle: ValueHandle) -> Option<&HandleEntry> {
        self.handles.get(handle)
    }

    pub(crate) fn take_handle(&mut self, handle: ValueHandle) -> Option<HandleEntry> {
        self.handles.take(handle)
    }

    pub(crate) fn take_managed_root(
        &mut self,
        handle: ValueHandle,
        rep: RuntimeRep,
    ) -> Option<OwnedManagedRoot> {
        self.handle(handle).filter(|entry| entry.rep == rep)?;
        let entry = self.take_handle(handle)?;
        Some(OwnedManagedRoot {
            cell: entry.slot,
            rep: entry.rep,
        })
    }

    pub(crate) fn rehome_handle(&mut self, handle: ValueHandle, realm: RealmId) -> bool {
        self.handles.rehome(handle, realm)
    }

    /// ROOT is the machine's own scope, not a closable resource scope: its handles,
    /// frames and cancellation flag live until the machine is torn down.
    /// Closing it releases nothing, whichever engine or session asks.
    pub(crate) fn close_realm(&mut self, realm: RealmId) -> ClosedRealm {
        if realm == RealmId::ROOT {
            return ClosedRealm {
                frames: Vec::new(),
                handles: Vec::new(),
            };
        }
        let frame_ids: Vec<_> = self
            .continuations
            .iter()
            .filter_map(|(&id, frame)| (frame.realm == realm).then_some(id))
            .collect();
        let frames = frame_ids
            .into_iter()
            .filter_map(|id| self.continuations.remove(&id))
            .collect();

        let handles = self.handles.take_realm(realm);

        self.cancel_flags.remove(&realm);
        ClosedRealm { frames, handles }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handle_records_the_representation_it_was_minted_with() {
        let mut ledger = ResourceLedger::default();
        let machine = std::rc::Rc::new(crate::machine_state::MachineState::new());
        // Representation bookkeeping needs no heap dereference; both cells
        // have their real independent registration/storage owner.
        let lifted = ledger.insert_handle(
            OwnedRootCell::new(&machine, std::ptr::null_mut()).unwrap(),
            RealmId::ROOT,
            RuntimeRep::LiftedRef,
        );
        let unlifted = ledger.insert_handle(
            OwnedRootCell::new(&machine, std::ptr::null_mut()).unwrap(),
            RealmId::ROOT,
            RuntimeRep::UnliftedRef,
        );
        assert_eq!(
            ledger.handle(lifted).map(|e| e.rep),
            Some(RuntimeRep::LiftedRef)
        );
        assert_eq!(
            ledger.handle(unlifted).map(|e| e.rep),
            Some(RuntimeRep::UnliftedRef)
        );
    }

    #[test]
    fn cancel_flags_are_stable_within_a_scope_and_isolated_between_scopes() {
        let mut ledger = ResourceLedger::default();
        let a = RealmId::fresh();
        let b = RealmId::fresh();

        let a1 = ledger.cancel_flag(a);
        let a2 = ledger.cancel_flag(a);
        let b1 = ledger.cancel_flag(b);

        assert!(Arc::ptr_eq(&a1, &a2));
        assert!(!Arc::ptr_eq(&a1, &b1));
        assert_eq!(ledger.counts().cancellation_scopes, 2);

        let closed = ledger.close_realm(a);
        assert!(closed.frames.is_empty());
        assert!(closed.handles.is_empty());
        assert_eq!(ledger.counts().cancellation_scopes, 1);
    }

    #[test]
    fn the_root_realm_is_not_closable() {
        let mut ledger = ResourceLedger::default();
        let root = ledger.cancel_flag(RealmId::ROOT);
        let closed = ledger.close_realm(RealmId::ROOT);
        assert!(closed.frames.is_empty());
        assert!(closed.handles.is_empty());
        assert!(Arc::ptr_eq(&root, &ledger.cancel_flag(RealmId::ROOT)));
        assert_eq!(ledger.counts().cancellation_scopes, 1);
    }
}
