//! Invocation-local descriptor retention uses the old-space lifetime.
//! Prepared GC and observation borrow their exact-start admission owner. The
//! invocation boxes this owner, clears the machine-state borrow at teardown,
//! and remains !Send so compiled-code custody stays on its owning thread.

use crate::{context::VMContext, host_fns::RuntimeError, machine_state::MachineState};
use std::collections::HashMap;
use std::sync::Arc;
use tidepool_heap::{
    descriptor_region::{DescriptorArena, DescriptorOldSpace, DescriptorSourceSpace},
    execution_descriptor::{DescriptorState, DescriptorTraceError, ObjectDescriptor},
    external_storage::ExternalPayloadOwner,
    gc::promotion::{compact_descriptor_arenas, promote_and_fixup_with_external, PromotionFailure},
    managed_reference::{tag_of, tag_valid, untag},
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

/// The arenas one compaction is evacuating. Source membership, unlike
/// [`Previous`]'s stable-target admission, reports an object this copy has
/// already forwarded as its own.
struct RetiringArenas<'a>(&'a [DescriptorArena]);

impl RetiringArenas<'_> {
    /// Whether `encoded` names storage inside the retiring arenas at all.
    /// Used to select which nursery fields are compaction roots; exactness is
    /// proven by the copier's own admission, not here.
    fn references_source(&self, encoded: usize) -> bool {
        let address = untag(encoded);
        self.0
            .iter()
            .any(|arena| arena.allocation_range().contains(&address))
    }

    /// External objects the source may expose, bounding the copier's payload
    /// scratch. Walked before any mutation.
    fn external_handles(&self) -> Result<usize, DescriptorTraceError> {
        let mut handles = 0;
        for arena in self.0 {
            arena.walk_sealed(|_, descriptor| {
                handles += usize::from(descriptor.external_kind().is_some());
                Ok(())
            })?;
        }
        Ok(handles)
    }
}

// SAFETY: compaction exclusively borrows the owning OldSpace while this view
// is used; it excludes the unfinished destination. Arenas never move their
// bytes, and their sealed starts/extents were proven by `DescriptorArena::seal`.
unsafe impl DescriptorSourceSpace for RetiringArenas<'_> {
    fn locate_start(&self, address: usize) -> Result<Option<usize>, DescriptorTraceError> {
        for arena in self.0 {
            if let Some(available) = arena.locate_start(address)? {
                return Ok(Some(available));
            }
        }
        Ok(None)
    }

    fn covers_slot(&self, address: usize) -> bool {
        self.0.iter().any(|arena| {
            let range = arena.allocation_range();
            let end = address.saturating_add(std::mem::size_of::<*mut u8>());
            address < range.end && range.start < end
        })
    }

    fn overlaps_range(&self, start: usize, end: usize) -> bool {
        self.0.iter().any(|arena| {
            let range = arena.allocation_range();
            start < range.end && range.start < end
        })
    }

    fn source_bytes(&self) -> usize {
        self.0.iter().map(DescriptorArena::bytes_used).sum()
    }
}

/// The live nursery as seen by a compaction: what stays put while old space
/// moves. Every exact object start found by the pre-copy walk, with the
/// descriptor that walk proved, so admission can validate state and tag the
/// way an arena does.
struct NurseryObjects {
    start: usize,
    end: usize,
    /// Descriptors are pinned by the machine's installed descriptor space,
    /// which the copy borrows mutably only for its scratch; no descriptor is
    /// added or removed while this view exists.
    objects: HashMap<usize, *const ObjectDescriptor>,
}

// SAFETY: the walk that built `objects` proved every start and its descriptor
// against the machine's installed descriptor space, and no mutator runs
// between that walk and the end of the copy that borrows this view.
unsafe impl DescriptorOldSpace for NurseryObjects {
    fn admit(&self, encoded: usize) -> Result<Option<usize>, DescriptorTraceError> {
        let address = untag(encoded);
        if address < self.start || address >= self.end {
            return Ok(None);
        }
        let descriptor = self
            .objects
            .get(&address)
            .ok_or(DescriptorTraceError::InvalidManagedPointer { address })?;
        // SAFETY: pinned for the view's life (see `objects`).
        let descriptor = unsafe { &**descriptor };
        // SAFETY: an exact start proven by the pre-copy walk, readable to the
        // end of the live nursery prefix.
        let state = unsafe { descriptor.state(address as *const u8, self.end - address) }?;
        if state == DescriptorState::Forwarded {
            return Err(DescriptorTraceError::ForwardedObject);
        }
        let tag = tag_of(encoded);
        if !tag_valid(tag, descriptor.kind(), state, descriptor.constructor_tag()) {
            return Err(DescriptorTraceError::InvalidManagedTag { address, tag });
        }
        Ok(Some(encoded))
    }
}

/// What one prepared-arena compaction reclaimed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct PreparedCompactionStats {
    pub before_bytes: usize,
    pub after_bytes: usize,
    pub reclaimed_bytes: usize,
}

impl super::OldSpace {
    /// Compact every prepared arena into one fresh arena holding exactly the
    /// live descriptor objects.
    ///
    /// This is the promotion copy with its source generalized: the retiring
    /// arenas replace the nursery range, and the NURSERY becomes the admitted
    /// stable space. `program_roots` are the installed programs' root-block
    /// words; the machine's complete root snapshot, and every nursery
    /// reference and external-payload slot that names old space, are joined
    /// here. A remembered slot inside a retiring arena is not a root: it
    /// travels with its object and is re-registered at the copied address.
    /// Every other remembered slot is, exactly as in promotion: a retained
    /// boxed-array payload remembers all of its slots, some of which name old
    /// objects, and its owner may already be dead (payloads are not swept from
    /// old space yet), so its slots must follow the move.
    ///
    /// # Safety
    /// No generated frame is live, no temporary root is registered and no
    /// observation borrows old space (the caller holds `Quiescent`).
    /// `machine`, `vmctx` and this OldSpace belong to one invocation, and
    /// every owner stays installed through the copy.
    pub(crate) unsafe fn compact_prepared(
        &mut self,
        machine: &MachineState,
        vmctx: &mut VMContext,
        program_roots: &[*mut *mut u8],
        descriptors: &[Arc<ObjectDescriptor>],
    ) -> Result<PreparedCompactionStats, RuntimeError> {
        let before_bytes = self.prepared_bytes_used();
        if self.prepared_arenas.is_empty() {
            return Ok(PreparedCompactionStats::default());
        }
        if before_bytes == 0 {
            // Empty arenas hold no object any slot could name.
            for arena in self.prepared_arenas.drain(..) {
                let range = arena.allocation_range();
                machine.retire_old_space_arena(range.start as *const u8, range.end as *const u8);
            }
            return Ok(PreparedCompactionStats::default());
        }
        if machine.prepared_call_status() != crate::prepared_control::CallStatus::Success {
            return Err(machine
                .current_failure()
                .map_or(crate::host_fns::bad_pointer(), |failure| failure.cause));
        }
        let snapshot = machine.complete_root_snapshot(&[]).into_slots();
        let mut state = machine
            .take_gc_state()
            .ok_or_else(|| crate::host_fns::bad_pointer())?;
        // Always restore the owning GcState, including terminal failure: a
        // partially forwarded source still holds live pointers.
        let outcome: Result<PreparedCompactionStats, RuntimeError> = (|| {
            let used = (vmctx.alloc_ptr as usize)
                .checked_sub(state.active_start as usize)
                .filter(|used| *used <= state.active_size && used % 8 == 0)
                .ok_or_else(|| crate::host_fns::bad_pointer())?;
            let prepared = state
                .prepared
                .as_mut()
                .ok_or_else(|| crate::host_fns::bad_pointer())?;

            // Every fallible step precedes the first forwarded header.
            let mut roots = Vec::new();
            roots
                .try_reserve(snapshot.len() + program_roots.len())
                .map_err(|_| RuntimeError::HeapOverflow)?;
            let nursery = {
                let source = RetiringArenas(&self.prepared_arenas);
                roots.extend(
                    snapshot
                        .iter()
                        .copied()
                        .filter(|&slot| !source.covers_slot(slot as usize)),
                );
                roots.extend_from_slice(program_roots);
                scan_nursery_edges(
                    &prepared.space,
                    machine,
                    state.active_start,
                    used,
                    &source,
                    &mut roots,
                )?
            };
            let external_handles = RetiringArenas(&self.prepared_arenas)
                .external_handles()
                .map_err(preparation_error)?;
            let retiring_ranges: Vec<(*const u8, *const u8)> = {
                let mut ranges = Vec::new();
                ranges
                    .try_reserve_exact(self.prepared_arenas.len())
                    .map_err(|_| RuntimeError::HeapOverflow)?;
                ranges.extend(self.prepared_arenas.iter().map(|arena| {
                    let range = arena.allocation_range();
                    (range.start as *const u8, range.end as *const u8)
                }));
                ranges
            };
            let arena = DescriptorArena::reserve(before_bytes, descriptors.iter().cloned())
                .map_err(preparation_error)?;
            self.prepared_arenas
                .try_reserve(1)
                .map_err(|_| RuntimeError::HeapOverflow)?;
            // Ownership/range publication precede mutation; a failed
            // destination remains owned but is never observed.
            let range = arena.allocation_range();
            machine.register_old_space_arena(range.start as *const u8, range.end as *const u8);
            machine.arm_write_barrier();
            self.prepared_arenas.push(arena);
            let last = self.prepared_arenas.len() - 1;
            let (retiring, destination) = self.prepared_arenas.split_at_mut(last);
            let copied = compact_descriptor_arenas(
                &roots,
                &RetiringArenas(retiring),
                external_handles,
                &mut destination[0],
                &mut prepared.space,
                Some(&nursery),
                machine,
            )
            .map_err(|error| match error {
                PromotionFailure::Preparation(error) => preparation_error(error),
                PromotionFailure::Incomplete(error) => RuntimeError::IncompletePromotion(error),
            })?;

            // The copy succeeded: the retiring arenas hold only forwarding
            // headers, so nothing below may leave them installed.
            for (start, end) in retiring_ranges {
                machine.retire_old_space_arena(start, end);
            }
            machine.bump_gc_generation();
            if copied.bytes_copied == 0 {
                // Nothing is live in old space: keep no empty arena either.
                let range = self.prepared_arenas[last].allocation_range();
                machine.retire_old_space_arena(range.start as *const u8, range.end as *const u8);
                self.prepared_arenas.clear();
                return Ok(PreparedCompactionStats {
                    before_bytes,
                    after_bytes: 0,
                    reclaimed_bytes: before_bytes,
                });
            }
            self.prepared_arenas.drain(..last);
            // Re-register the old-to-young edges at their copied addresses.
            // `retire_old_space_arena` forgot the source copies; edges out of
            // external payloads were never in a retiring range and stand.
            self.prepared_arenas[0]
                .walk_sealed(|object, descriptor| {
                    if descriptor.external_kind().is_some() {
                        return Ok(());
                    }
                    let extent = descriptor.allocation_extent() as usize;
                    descriptor.for_each_trace_slot(object, extent, |slot| {
                        let value = untag(std::ptr::read(slot) as usize);
                        if value >= nursery.start && value < nursery.end {
                            machine.register_remembered_slot(slot);
                        }
                    })
                })
                .map_err(RuntimeError::IncompletePromotion)?;
            let after_bytes = copied.bytes_copied;
            Ok(PreparedCompactionStats {
                before_bytes,
                after_bytes,
                reclaimed_bytes: before_bytes.saturating_sub(after_bytes),
            })
        })();
        machine.put_gc_state(state);
        if let Err(cause) = &outcome {
            machine.set_first_cause(cause.clone());
        }
        outcome
    }

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
        // Every fallible bookkeeping step is completed before promotion
        // mutates the nursery.  After promotion only infallible Box ownership
        // publication remains, so a multi-result transfer is all-or-nothing
        // from the caller's perspective.
        let mut retained = Vec::new();
        retained
            .try_reserve_exact(selected.len())
            .map_err(|_| RuntimeError::HeapOverflow)?;
        self.slots
            .try_reserve(selected.len())
            .map_err(|_| RuntimeError::HeapOverflow)?;
        for &source in selected {
            if source.is_null() || (*source).is_null() {
                return Err(crate::host_fns::bad_pointer());
            }
        }
        self.promote_prepared(machine, vmctx, selected, descriptors)?;
        for &source in selected {
            let pointer = *source;
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
                .current_failure()
                .map_or(crate::host_fns::bad_pointer(), |failure| failure.cause));
        }
        let (active_start, active_size) = machine
            .gc_active_range()
            .ok_or_else(|| crate::host_fns::bad_pointer())?;
        let active_end = (active_start as usize)
            .checked_add(active_size)
            .ok_or_else(|| crate::host_fns::bad_pointer())?;
        // Static and previously retained results already have stable owners;
        // retention promotion is a no-op for them and must not manufacture an
        // empty descriptor arena.
        let mut already_stable = true;
        for &slot in selected {
            let encoded = *slot as usize;
            if encoded == 0 {
                return Err(crate::host_fns::bad_pointer());
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
            return Err(crate::host_fns::bad_pointer());
        }
        if already_stable {
            return Ok(());
        }
        let roots = machine.complete_root_snapshot(&[]).into_slots();
        let mut state = machine
            .take_gc_state()
            .ok_or_else(|| crate::host_fns::bad_pointer())?;
        // Always restore the owning GcState, including terminal failure. Its
        // semispaces may both contain live pointers after partial forwarding.
        let outcome: Result<(), RuntimeError> = (|| {
            let used = (vmctx.alloc_ptr as usize)
                .checked_sub(state.active_start as usize)
                .filter(|used| *used <= state.active_size && used % 8 == 0)
                .ok_or_else(|| crate::host_fns::bad_pointer())?;
            let active = state
                .active_buffer
                .as_mut()
                .ok_or_else(|| crate::host_fns::bad_pointer())?;
            let prepared = state
                .prepared
                .as_mut()
                .ok_or_else(|| crate::host_fns::bad_pointer())?;
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

/// Walk the live nursery prefix by exact starts (the walk the ordinary
/// collector's preparation performs), recording each object for admission and
/// appending every reference or external-payload slot that currently names
/// retiring old space to `roots`. Nothing is mutated.
///
/// # Safety
/// `start..start+used` is the machine's initialized live nursery prefix and
/// `space` its installed descriptor space.
unsafe fn scan_nursery_edges(
    space: &tidepool_heap::gc::raw::DescriptorSpace,
    external: &dyn ExternalPayloadOwner,
    start: *mut u8,
    used: usize,
    source: &RetiringArenas<'_>,
    roots: &mut Vec<*mut *mut u8>,
) -> Result<NurseryObjects, RuntimeError> {
    let base = start as usize;
    let end = base
        .checked_add(used)
        .ok_or_else(|| crate::host_fns::bad_pointer())?;
    let mut objects = HashMap::new();
    let mut offset = 0;
    while offset < used {
        let object = start.add(offset);
        let identity = object.cast::<usize>().read() & !7;
        let descriptor = space
            .live_descriptor(identity)
            .ok_or_else(|| crate::host_fns::bad_pointer())?;
        let extent = descriptor.allocation_extent() as usize;
        if extent < 16 || !extent.is_multiple_of(8) || extent > used - offset {
            return Err(crate::host_fns::bad_pointer());
        }
        if let Some(kind) = descriptor.external_kind() {
            let payload = descriptor
                .external_payload_slot(object, extent)
                .map_err(preparation_error)?
                .read();
            let slots = external
                .slots(payload, kind)
                .map_err(|error| preparation_error(DescriptorTraceError::ExternalPayload(error)))?;
            roots
                .try_reserve(slots.len())
                .map_err(|_| RuntimeError::HeapOverflow)?;
            for slot in slots {
                if source.references_source(slot.read() as usize) {
                    roots.push(slot);
                }
            }
        } else {
            let mut overflow = false;
            descriptor
                .for_each_trace_slot(object, extent, |slot| {
                    if source.references_source(slot.read() as usize) {
                        if roots.try_reserve(1).is_err() {
                            overflow = true;
                        } else {
                            roots.push(slot);
                        }
                    }
                })
                .map_err(preparation_error)?;
            if overflow {
                return Err(RuntimeError::HeapOverflow);
            }
        }
        objects
            .try_reserve(1)
            .map_err(|_| RuntimeError::HeapOverflow)?;
        objects.insert(object as usize, std::ptr::from_ref(descriptor));
        offset += extent;
    }
    Ok(NurseryObjects {
        start: base,
        end,
        objects,
    })
}

fn preparation_error(error: DescriptorTraceError) -> RuntimeError {
    match error {
        DescriptorTraceError::MetadataAllocation => RuntimeError::HeapOverflow,
        _ => crate::host_fns::bad_pointer(),
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
        let mut vmctx = unsafe { VMContext::new(start, start.add(size)) };
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

    /// Compaction over boxed-array edges in both directions: an old wrapper
    /// whose retained payload names a NURSERY child keeps that edge (and its
    /// remembered slot) untouched, while a nursery wrapper whose young
    /// payload names an OLD child has that slot rewritten to the copy. The
    /// unreachable old leaf is reclaimed and exactly one arena remains.
    #[test]
    fn compaction_follows_payload_edges_between_old_and_nursery() {
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
        let descriptors = [wrapper.clone(), leaf.clone()];
        let w = wrapper.allocation_extent() as usize;
        let l = leaf.allocation_extent() as usize;
        let tagged = |object: *mut u8| (object as usize | usize::from(leaf.tag())) as *mut u8;
        let element = |payload: *mut u8| unsafe { payload.add(8).cast::<*mut u8>() };
        let machine = MachineState::new();
        machine
            .install_prepared_buffer(vec![0_u64; 64], descriptors.to_vec())
            .unwrap();
        let (start, size) = machine.gc_active_range().unwrap();

        // Old-bound graph: wrapper_o -> payload_o -> leaf_o, plus a leaf that
        // becomes garbage once promoted.
        let (leaf_o, leaf_g) = unsafe { (start.add(w), start.add(w + l)) };
        unsafe {
            wrapper.initialize_header(start);
            leaf.initialize_header(leaf_o);
            leaf.initialize_header(leaf_g);
        }
        let payload_o = machine
            .allocate_external_storage(ExternalStorageKind::BoxedArray, 1)
            .unwrap();
        machine
            .store_external_element(payload_o, 0, tagged(leaf_o))
            .unwrap();
        unsafe {
            wrapper
                .external_payload_slot(start, w)
                .unwrap()
                .write(payload_o);
        }
        let mut root_o = start;
        let mut root_g = tagged(leaf_g);
        machine.register_rust_root(&mut root_o);
        machine.register_rust_root(&mut root_g);
        let mut vmctx = unsafe { VMContext::new(start, start.add(size)) };
        vmctx.machine_state = &machine as *const _ as *mut _;
        vmctx.alloc_ptr = unsafe { start.add(w + 2 * l) };
        let mut old = super::super::OldSpace::new();
        unsafe {
            old.promote_prepared(
                &machine,
                &mut vmctx,
                &[&mut root_o, &mut root_g],
                &descriptors,
            )
        }
        .unwrap();
        assert_eq!(old.prepared_bytes_used(), w + 2 * l);
        root_g = std::ptr::null_mut();
        let promoted_leaf = unsafe { element(payload_o).read() };

        // Nursery graph: wrapper_n -> payload_n -> promoted leaf_o, and the
        // old payload now names a fresh nursery leaf.
        let cursor = vmctx.alloc_ptr;
        let (leaf_n, wrapper_n) = unsafe { (cursor, cursor.add(l)) };
        unsafe {
            leaf.initialize_header(leaf_n);
            wrapper.initialize_header(wrapper_n);
        }
        let payload_n = machine
            .allocate_external_storage(ExternalStorageKind::BoxedArray, 1)
            .unwrap();
        machine
            .store_external_element(payload_n, 0, promoted_leaf)
            .unwrap();
        unsafe {
            wrapper
                .external_payload_slot(wrapper_n, w)
                .unwrap()
                .write(payload_n);
        }
        machine
            .store_external_element(payload_o, 0, tagged(leaf_n))
            .unwrap();
        let mut root_n = wrapper_n;
        machine.register_rust_root(&mut root_n);
        vmctx.alloc_ptr = unsafe { cursor.add(l + w) };

        let stats = unsafe { old.compact_prepared(&machine, &mut vmctx, &[], &descriptors) }
            .expect("compaction succeeds");
        assert_eq!(
            stats,
            PreparedCompactionStats {
                before_bytes: w + 2 * l,
                after_bytes: w + l,
                reclaimed_bytes: l,
            }
        );
        assert_eq!(old.prepared_arenas.len(), 1);
        let range = old.prepared_arenas[0].allocation_range();
        assert_eq!(
            machine.old_space_arena_ranges(),
            vec![(range.start as *const u8, range.end as *const u8)]
        );
        assert!(range.contains(&(root_o as usize)));
        assert_eq!(root_n, wrapper_n, "the nursery does not move");
        let copied_leaf = unsafe { element(payload_n).read() } as usize;
        assert!(range.contains(&untag(copied_leaf)));
        assert_ne!(copied_leaf, promoted_leaf as usize);
        assert_eq!(tag_of(copied_leaf), leaf.tag());
        assert_eq!(
            unsafe { leaf.state(untag(copied_leaf) as *const u8, l) }.unwrap(),
            DescriptorState::Live
        );
        assert_eq!(unsafe { element(payload_o).read() }, tagged(leaf_n));
        assert!(machine
            .remembered_slots_snapshot()
            .contains(&element(payload_o)));
        assert!(root_g.is_null());
        machine.clear_gc_state();
    }
}
