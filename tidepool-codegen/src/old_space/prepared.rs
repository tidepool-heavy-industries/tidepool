//! Invocation-local descriptor retention uses OldSpace's lifetime, not the
//! legacy Core scanner. Prepared arenas are isolated from Core compaction;
//! ordinary prepared GC and observation borrow their exact-start admission
//! owner. The invocation boxes this owner, clears the MachineState borrow at
//! teardown, and remains !Send so compiled-code custody is never widened into
//! OldSpace's legacy unsafe-Send boundary.

use crate::{context::VMContext, host_fns::RuntimeError, machine_state::MachineState};
use std::sync::Arc;
use tidepool_heap::{
    descriptor_region::{DescriptorArena, DescriptorOldSpace},
    execution_descriptor::{DescriptorTraceError, ObjectDescriptor},
    gc::promotion::{promote_and_fixup_with_external, PromotionFailure},
};

struct Previous<'a>(&'a [DescriptorArena]);

// SAFETY: promotion exclusively borrows the owning OldSpace while this view is
// used; it excludes the unfinished destination. Arenas never move their bytes.
unsafe impl DescriptorOldSpace for Previous<'_> {
    fn admit(&self, encoded: usize) -> Result<Option<usize>, DescriptorTraceError> {
        for arena in self.0 {
            if let Some(reference) = arena.admit(encoded)? {
                return Ok(Some(reference));
            }
        }
        Ok(None)
    }
}

// SAFETY: prepared arenas are owned by this OldSpace and remain allocated for
// the invocation lifetime. Ordinary prepared collection and observation only
// borrow this owner while no promotion mutates the arena vector.
unsafe impl DescriptorOldSpace for super::OldSpace {
    fn admit(&self, encoded: usize) -> Result<Option<usize>, DescriptorTraceError> {
        for arena in &self.prepared_arenas {
            if let Some(reference) = arena.admit(encoded)? {
                return Ok(Some(reference));
            }
        }
        Ok(None)
    }
}

impl super::OldSpace {
    /// Promote selected prepared roots and give each one a stable,
    /// persistently registered slot owned by this old-space owner.
    ///
    /// The returned slots are the only representation a handle ledger may
    /// retain.  Callers must never retain the temporary result/argument slot:
    /// that storage belongs to one execution frame and is removed at unwind.
    pub(crate) unsafe fn retain_prepared(
        &mut self,
        machine: &MachineState,
        vmctx: &mut VMContext,
        selected: &[*mut *mut u8],
        descriptors: &[Arc<ObjectDescriptor>],
    ) -> Result<Vec<super::RootSlot>, RuntimeError> {
        self.promote_prepared(machine, vmctx, selected, descriptors)?;
        let mut retained = Vec::new();
        retained
            .try_reserve_exact(selected.len())
            .map_err(|_| RuntimeError::HeapOverflow)?;
        for &source in selected {
            let pointer = *source;
            if pointer.is_null() {
                return Err(RuntimeError::BadPointer);
            }
            let mut cell = Box::new(pointer);
            let address: *mut *mut u8 = &mut *cell;
            self.slots.push(cell);
            machine.register_persistent_root(address);
            retained.push(super::RootSlot::new(address));
        }
        Ok(retained)
    }

    /// # Safety
    /// No generated frames are live. Selected slots belong to the complete
    /// invocation root registry; vmctx/machine/OldSpace belong to that same
    /// invocation. Every owner remains installed through both copying phases.
    pub(crate) unsafe fn promote_prepared(
        &mut self,
        machine: &MachineState,
        vmctx: &mut VMContext,
        selected: &[*mut *mut u8],
        descriptors: &[Arc<ObjectDescriptor>],
    ) -> Result<(), RuntimeError> {
        if machine.prepared_call_status() != crate::prepared_control::CallStatus::Success {
            return Err(machine
                .last_failure()
                .map_or(RuntimeError::BadPointer, |failure| failure.cause));
        }
        let (active_start, active_size) =
            machine.gc_active_range().ok_or(RuntimeError::BadPointer)?;
        let active_end = (active_start as usize)
            .checked_add(active_size)
            .ok_or(RuntimeError::BadPointer)?;
        // Static and previously retained results already have stable owners;
        // retention promotion is a no-op for them and must not manufacture an
        // empty descriptor arena.
        let mut already_stable = true;
        for &slot in selected {
            let encoded = *slot as usize;
            if encoded == 0 {
                return Err(RuntimeError::BadPointer);
            }
            let address = tidepool_heap::managed_reference::untag(encoded);
            if address >= active_start as usize && address < active_end {
                already_stable = false;
                continue;
            }
            if self.admit(encoded).map_err(preparation_error)?.is_some()
                || machine
                    .prepared_static_reference(encoded)
                    .map_err(preparation_error)?
                    .is_some()
            {
                continue;
            }
            return Err(RuntimeError::BadPointer);
        }
        if already_stable {
            return Ok(());
        }
        let roots = machine
            .complete_root_snapshot(&[], &mut vmctx.tail_callee, &mut vmctx.tail_arg)
            .into_slots();
        let mut state = machine.take_gc_state().ok_or(RuntimeError::BadPointer)?;
        // Always restore the owning GcState, including terminal failure. Its
        // semispaces may both contain live pointers after partial forwarding.
        let outcome: Result<(), RuntimeError> = (|| {
            let used = (vmctx.alloc_ptr as usize)
                .checked_sub(state.active_start as usize)
                .filter(|used| *used <= state.active_size && used % 8 == 0)
                .ok_or(RuntimeError::BadPointer)?;
            let active = state
                .active_buffer
                .as_mut()
                .ok_or(RuntimeError::BadPointer)?;
            let prepared = state.prepared.as_mut().ok_or(RuntimeError::BadPointer)?;
            if prepared.spare.len() < active.len() {
                prepared
                    .spare
                    .try_reserve_exact(active.len() - prepared.spare.len())
                    .map_err(|_| RuntimeError::HeapOverflow)?;
                prepared.spare.resize(active.len(), 0);
            }
            let arena = DescriptorArena::reserve(used, descriptors.iter().cloned())
                .map_err(preparation_error)?;
            self.prepared_arenas
                .try_reserve(1)
                .map_err(|_| RuntimeError::HeapOverflow)?;
            // Ownership/range publication precede mutation; a failed destination
            // remains owned but is never observed by a terminal invocation.
            let range = arena.allocation_range();
            machine.register_old_space_arena(range.start as *const u8, range.end as *const u8);
            machine.arm_write_barrier();
            self.prepared_arenas.push(arena);
            let last = self.prepared_arenas.len() - 1;
            let (previous, destination) = self.prepared_arenas.split_at_mut(last);
            let spare = std::slice::from_raw_parts_mut(
                prepared.spare.as_mut_ptr().cast::<u8>(),
                prepared.spare.len() * 8,
            );
            let copied = promote_and_fixup_with_external(
                selected,
                &roots,
                state.active_start,
                used,
                spare,
                &mut destination[0],
                &mut prepared.space,
                Some(&Previous(previous)),
                machine,
            )
            .map_err(|error| match error {
                PromotionFailure::Preparation(error) => preparation_error(error),
                PromotionFailure::Incomplete(error) => RuntimeError::IncompletePromotion(error),
            })?;
            // Publish the consistent nursery before retention bookkeeping.
            // Any subsequent bookkeeping failure is terminal and preserves
            // every heap/payload owner through unwind.
            std::mem::swap(active, &mut prepared.spare);
            state.active_start = active.as_mut_ptr().cast();
            state.active_size = active.len() * 8;
            prepared.used = copied.nursery_bytes;
            vmctx.alloc_ptr = state.active_start.add(copied.nursery_bytes);
            vmctx.alloc_limit = state.active_start.add(state.active_size);
            machine.bump_gc_generation();
            machine
                .retain_external_payloads(&copied.promoted_external_payloads)
                .map_err(|error| {
                    RuntimeError::IncompletePromotion(DescriptorTraceError::ExternalPayload(error))
                })?;
            Ok(())
        })();
        machine.put_gc_state(state);
        if let Err(cause) = &outcome {
            machine.set_first_cause(cause.clone());
        }
        outcome
    }
}

fn preparation_error(error: DescriptorTraceError) -> RuntimeError {
    match error {
        DescriptorTraceError::MetadataAllocation => RuntimeError::HeapOverflow,
        _ => RuntimeError::BadPointer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use tidepool_heap::external_storage::ExternalStorageKind;
    use tidepool_repr::execution_schema::{
        Architecture, Endianness, StorageLayout, TargetDescriptor,
    };

    #[test]
    fn promotion_retains_external_payload_after_publishing_consistent_nursery() {
        let target = TargetDescriptor {
            architecture: Architecture::X86_64,
            endianness: Endianness::Little,
            pointer_width: 64,
            word_width: 64,
            abi: "sysv64".into(),
            features: Vec::new(),
        };
        let wrapper =
            Arc::new(ObjectDescriptor::external(ExternalStorageKind::BoxedArray, &target).unwrap());
        let leaf = Arc::new(
            ObjectDescriptor::constructor(1, StorageLayout::for_reps(&target, &[]).unwrap(), None)
                .unwrap(),
        );
        let used = wrapper.allocation_extent() as usize + leaf.allocation_extent() as usize;
        let machine = MachineState::new();
        machine
            .install_prepared_buffer(vec![0_u64; used / 8], vec![wrapper.clone(), leaf.clone()])
            .unwrap();
        let (start, size) = machine.gc_active_range().unwrap();
        let child = unsafe { start.add(wrapper.allocation_extent() as usize) };
        unsafe {
            wrapper.initialize_header(start);
            leaf.initialize_header(child);
        }
        let payload = machine
            .allocate_external_storage(ExternalStorageKind::BoxedArray, 1)
            .unwrap();
        machine
            .store_external_element(
                payload,
                0,
                (child as usize | usize::from(leaf.tag())) as *mut u8,
            )
            .unwrap();
        unsafe {
            wrapper
                .external_payload_slot(start, wrapper.allocation_extent() as usize)
                .unwrap()
                .write(payload);
        }
        let mut root = start;
        machine.register_rust_root(&mut root);
        let mut vmctx =
            unsafe { VMContext::new(start, start.add(size), crate::host_fns::gc_trigger) };
        vmctx.machine_state = &machine as *const _ as *mut _;
        vmctx.alloc_ptr = unsafe { start.add(used) };
        let mut old = super::super::OldSpace::new();
        unsafe {
            old.promote_prepared(
                &machine,
                &mut vmctx,
                &[&mut root],
                &[wrapper.clone(), leaf.clone()],
            )
        }
        .unwrap();
        assert_eq!(machine.remembered_slots_count(), 1);
        machine
            .commit_external_sweep(machine.plan_external_minor_sweep(&HashSet::new()).unwrap())
            .unwrap();
        assert_eq!(machine.external_storage_stats().live_objects, 1);
        assert!(!root.is_null());
        machine.clear_gc_state();
    }
}
