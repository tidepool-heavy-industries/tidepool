//! Runtime-resource ownership for one JIT machine.
//!
//! This module owns identities and scope membership for parked continuations,
//! live value handles, and scope-local cancellation flags. It deliberately
//! does not register or deregister GC roots: those operations require the
//! machine's [`crate::machine_state::MachineState`] and remain at the JIT
//! boundary. Scope closure removes all matching entries here first and hands
//! their rooted payloads back to that boundary for exact settlement.

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
use tidepool_repr::{DataConTable, PrincipalId};

use crate::old_space::RootSlot;
use crate::prepared_program::ProgramId;
use crate::suspension::{ContinuationId, ParkKind, RealmId, ValueHandle};
use tidepool_repr::execution_schema::{RuntimeRep, ValueId};

/// The GC-tracked cell holding a parked continuation's heap pointer. Both
/// shapes are registered as stowed roots for the frame's whole parked
/// lifetime; the collector rewrites the cell in place on every collection.
pub(crate) enum FrameCell {
    /// Core: a heap-stable `Box` cell minted at park time.
    Boxed(Box<*mut u8>),
    /// Prepared: the continuation handle's own `OldSpace` root slot, moved
    /// from the machine's persistent-root list to its stowed-root list for
    /// the park. The slot cell stays with `OldSpace` for the machine's life;
    /// only its registration moves.
    Slot(RootSlot),
}

impl FrameCell {
    /// The slot address the collector reads and rewrites.
    pub(crate) fn slot(&self) -> *mut *mut u8 {
        match self {
            Self::Boxed(cell) => std::ptr::from_ref::<*mut u8>(&**cell).cast_mut(),
            Self::Slot(slot) => slot.addr(),
        }
    }
}

/// What a resume needs to interpret the answer and re-enter a prepared
/// continuation. The evidence owner and the runner can be different
/// programs: a retained closure from an earlier turn may reach a site that
/// turn declared, while the turn that invoked it owns the resume entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreparedFrameEvidence {
    /// The installed program whose site and type tables describe `site`.
    pub owner: ProgramId,
    /// The typed site the suspended request named.
    pub site: u64,
    /// The program whose admitted resume entry re-enters the continuation.
    pub runner: ProgramId,
    /// `runner`'s `__resume` top: `\q x -> settle (resumeLifted q x)`.
    pub resume_entry: ValueId,
    /// The representation the continuation was retained with.
    pub continuation_rep: RuntimeRep,
}

/// Per-engine evidence a parked frame carries for its eventual resume.
pub(crate) enum FrameEvidence {
    /// Core decodes answers and requests through the session's constructor
    /// snapshot, refreshed as later fragments extend the vocabulary.
    Core(Arc<DataConTable>),
    /// Prepared validates answers against installed site evidence.
    Prepared(PreparedFrameEvidence),
}

/// One parked continuation and all policy needed to resume it.
pub(crate) struct ContinuationFrame {
    pub(crate) cell: FrameCell,
    pub(crate) realm: RealmId,
    pub(crate) principal: PrincipalId,
    pub(crate) effect_policy: EffectRunPolicy,
    pub(crate) kind: ParkKind,
    pub(crate) live_payload_root: Option<RootSlot>,
    pub(crate) live_payload: LivePayloadPolicy,
    pub(crate) cancel_flag: Arc<AtomicBool>,
    pub(crate) evidence: FrameEvidence,
}

/// One live handle's rooted slot, cleanup owner, and the representation of
/// the word its slot holds. Core mints only lifted heap pointers; the
/// prepared machine also mints `UnliftedRef` handles (byte arrays, boxed
/// arrays), and a bare-handle delivery must recover that representation from
/// the ledger, not assume it.
pub(crate) struct HandleEntry {
    pub(crate) slot: RootSlot,
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
    /// Released when its owner releases it, or with its realm.
    Value,
    /// An installed program's top, retained for the machine's lifetime so
    /// later programs can import it instead of compiling their own copy.
    /// Owned by no realm's lifetime and never taken by scope closure.
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
        slot: RootSlot,
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

    pub(crate) fn holds_root(&self, slot: RootSlot) -> bool {
        self.handles
            .values()
            .any(|entry| std::ptr::eq(entry.slot.addr(), slot.addr()))
    }

    /// Every live handle's root slot.
    pub(crate) fn slots(&self) -> impl Iterator<Item = RootSlot> + '_ {
        self.handles.values().map(|entry| entry.slot)
    }

    /// Every [`HandleClass::Value`] handle owned by `realm`. A code export
    /// outlives every realm -- it is the machine's own root on installed
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

/// The process-local ownership ledger for one JIT machine.
#[derive(Default)]
pub(crate) struct ResourceLedger {
    continuations: HashMap<ContinuationId, ContinuationFrame>,
    next_continuation_id: u64,
    handles: RootHandleLedger,
    cancel_flags: HashMap<RealmId, Arc<AtomicBool>>,
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
                let evidence = match &frame.evidence {
                    FrameEvidence::Prepared(evidence) => Some(evidence),
                    FrameEvidence::Core(_) => None,
                };
                let payload = frame.live_payload_root.map(|root| (root.addr(), evidence));
                std::iter::once((frame.cell.slot(), evidence)).chain(payload)
            })
            .collect()
    }

    pub(crate) fn parked_ids(&self) -> Vec<ContinuationId> {
        let mut ids: Vec<_> = self.continuations.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// Replace every parked Core frame's constructor view with one already
    /// validated, monotone session-table snapshot.
    ///
    /// A live closure compiled by a later resident turn can be delivered into
    /// an older continuation. That continuation must then interpret effect
    /// responses using the whole session's constructor vocabulary, not only
    /// the vocabulary present when the frame first parked. Prepared frames
    /// carry installed site evidence instead and are left untouched.
    pub(crate) fn refresh_continuation_tables(&mut self, table: Arc<DataConTable>) {
        for frame in self.continuations.values_mut() {
            if let FrameEvidence::Core(current) = &mut frame.evidence {
                *current = Arc::clone(&table);
            }
        }
    }

    pub(crate) fn try_reserve_handles(
        &mut self,
        additional: usize,
    ) -> Result<(), std::collections::TryReserveError> {
        self.handles.try_reserve(additional)
    }

    /// Mint a handle for `slot`, recording `rep` as the representation of the
    /// word the slot holds (see [`HandleEntry::rep`]).
    pub(crate) fn insert_handle(
        &mut self,
        slot: RootSlot,
        realm: RealmId,
        rep: RuntimeRep,
    ) -> ValueHandle {
        self.handles.insert(slot, realm, rep, HandleClass::Value)
    }

    /// [`Self::insert_handle`] for a machine-lifetime export root: same
    /// handle namespace, counted as [`HandleClass::CodeExport`].
    pub(crate) fn insert_export_handle(&mut self, slot: RootSlot, rep: RuntimeRep) -> ValueHandle {
        self.handles
            .insert(slot, RealmId::ROOT, rep, HandleClass::CodeExport)
    }

    pub(crate) fn handle(&self, handle: ValueHandle) -> Option<&HandleEntry> {
        self.handles.get(handle)
    }

    pub(crate) fn take_handle(&mut self, handle: ValueHandle) -> Option<HandleEntry> {
        self.handles.take(handle)
    }

    pub(crate) fn rehome_handle(&mut self, handle: ValueHandle, realm: RealmId) -> bool {
        self.handles.rehome(handle, realm)
    }

    pub(crate) fn handle_holds_root(&self, slot: RootSlot) -> bool {
        self.handles.holds_root(slot)
    }

    /// ROOT is the machine's own scope, not a closable realm: its handles,
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

    pub(crate) fn drain_continuations(&mut self) -> Vec<ContinuationFrame> {
        self.continuations.drain().map(|(_, frame)| frame).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handle_records_the_representation_it_was_minted_with() {
        let mut ledger = ResourceLedger::default();
        let mut cell: *mut u8 = std::ptr::null_mut();
        // SAFETY: the ledger only stores the slot address here; nothing reads
        // through it in this test, and `cell` outlives the ledger.
        let slot = unsafe { RootSlot::new(&mut cell) };
        let lifted = ledger.insert_handle(slot, RealmId::ROOT, RuntimeRep::LiftedRef);
        let unlifted = ledger.insert_handle(slot, RealmId::ROOT, RuntimeRep::UnliftedRef);
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

    #[test]
    fn parked_frames_receive_one_shared_newer_constructor_table() {
        let mut ledger = ResourceLedger::default();
        let realm = RealmId::fresh();
        let original = Arc::new(DataConTable::new());
        for value in [std::ptr::null_mut(), std::ptr::dangling_mut()] {
            ledger.park(ContinuationFrame {
                cell: FrameCell::Boxed(Box::new(value)),
                realm,
                principal: tidepool_repr::PrincipalId::SYSTEM,
                effect_policy: EffectRunPolicy::SuspendAll,
                kind: ParkKind::Plain,
                live_payload_root: None,
                live_payload: LivePayloadPolicy::None,
                cancel_flag: Arc::new(AtomicBool::new(false)),
                evidence: FrameEvidence::Core(Arc::clone(&original)),
            });
        }

        let current = Arc::new(DataConTable::new());
        ledger.refresh_continuation_tables(Arc::clone(&current));

        for id in ledger.parked_ids() {
            let FrameEvidence::Core(table) =
                &ledger.continuation(id).expect("parked frame").evidence
            else {
                panic!("a Core frame keeps Core evidence");
            };
            assert!(Arc::ptr_eq(table, &current));
        }
    }
}
