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
use crate::suspension::{ContinuationId, ParkKind, RealmId, ValueHandle};

/// One parked continuation and all policy needed to resume it.
pub(crate) struct ContinuationFrame {
    pub(crate) cell: Box<*mut u8>,
    pub(crate) realm: RealmId,
    pub(crate) principal: PrincipalId,
    pub(crate) effect_policy: EffectRunPolicy,
    pub(crate) kind: ParkKind,
    pub(crate) live_payload_root: Option<RootSlot>,
    pub(crate) live_payload: LivePayloadPolicy,
    pub(crate) cancel_flag: Arc<AtomicBool>,
    pub(crate) table: Arc<DataConTable>,
}

/// One live handle's rooted slot and cleanup owner.
pub(crate) struct HandleEntry {
    pub(crate) slot: RootSlot,
    pub(crate) realm: RealmId,
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
    pub cancellation_scopes: usize,
}

/// The process-local ownership ledger for one JIT machine.
#[derive(Default)]
pub(crate) struct ResourceLedger {
    continuations: HashMap<ContinuationId, ContinuationFrame>,
    next_continuation_id: u64,
    handles: HashMap<u64, HandleEntry>,
    cancel_flags: HashMap<RealmId, Arc<AtomicBool>>,
}

impl ResourceLedger {
    pub(crate) fn counts(&self) -> ResourceCounts {
        ResourceCounts {
            parked_continuations: self.continuations.len(),
            value_handles: self.handles.len(),
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

    pub(crate) fn parked_ids(&self) -> Vec<ContinuationId> {
        let mut ids: Vec<_> = self.continuations.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// Replace every parked frame's constructor view with one already
    /// validated, monotone session-table snapshot.
    ///
    /// A live closure compiled by a later resident turn can be delivered into
    /// an older continuation. That continuation must then interpret effect
    /// responses using the whole session's constructor vocabulary, not only
    /// the vocabulary present when the frame first parked.
    pub(crate) fn refresh_continuation_tables(&mut self, table: Arc<DataConTable>) {
        for frame in self.continuations.values_mut() {
            frame.table = Arc::clone(&table);
        }
    }

    pub(crate) fn insert_handle(&mut self, slot: RootSlot, realm: RealmId) -> ValueHandle {
        let handle = ValueHandle::fresh();
        let replaced = self.handles.insert(handle.0, HandleEntry { slot, realm });
        debug_assert!(replaced.is_none(), "fresh value handle must not collide");
        handle
    }

    pub(crate) fn handle(&self, handle: ValueHandle) -> Option<&HandleEntry> {
        self.handles.get(&handle.0)
    }

    pub(crate) fn take_handle(&mut self, handle: ValueHandle) -> Option<HandleEntry> {
        self.handles.remove(&handle.0)
    }

    pub(crate) fn rehome_handle(&mut self, handle: ValueHandle, realm: RealmId) -> bool {
        let Some(entry) = self.handles.get_mut(&handle.0) else {
            return false;
        };
        entry.realm = realm;
        true
    }

    pub(crate) fn handle_holds_root(&self, slot: RootSlot) -> bool {
        self.handles
            .values()
            .any(|entry| std::ptr::eq(entry.slot.addr(), slot.addr()))
    }

    pub(crate) fn close_realm(&mut self, realm: RealmId) -> ClosedRealm {
        let frame_ids: Vec<_> = self
            .continuations
            .iter()
            .filter_map(|(&id, frame)| (frame.realm == realm).then_some(id))
            .collect();
        let frames = frame_ids
            .into_iter()
            .filter_map(|id| self.continuations.remove(&id))
            .collect();

        let handle_ids: Vec<_> = self
            .handles
            .iter()
            .filter_map(|(&id, entry)| (entry.realm == realm).then_some(id))
            .collect();
        let handles = handle_ids
            .into_iter()
            .filter_map(|id| self.handles.remove(&id))
            .collect();

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
    fn parked_frames_receive_one_shared_newer_constructor_table() {
        let mut ledger = ResourceLedger::default();
        let realm = RealmId::fresh();
        let original = Arc::new(DataConTable::new());
        for value in [std::ptr::null_mut(), std::ptr::dangling_mut()] {
            ledger.park(ContinuationFrame {
                cell: Box::new(value),
                realm,
                principal: tidepool_repr::PrincipalId::SYSTEM,
                effect_policy: EffectRunPolicy::SuspendAll,
                kind: ParkKind::Plain,
                live_payload_root: None,
                live_payload: LivePayloadPolicy::None,
                cancel_flag: Arc::new(AtomicBool::new(false)),
                table: Arc::clone(&original),
            });
        }

        let current = Arc::new(DataConTable::new());
        ledger.refresh_continuation_tables(Arc::clone(&current));

        for id in ledger.parked_ids() {
            assert!(Arc::ptr_eq(
                &ledger.continuation(id).expect("parked frame").table,
                &current
            ));
        }
    }
}
