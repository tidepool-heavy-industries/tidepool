//! Cheney's semi-space copying GC for raw HeapObjects.

use crate::execution_descriptor::{
    DescriptorState, DescriptorTraceError, ObjectDescriptor, ObjectKind,
};
use crate::external_storage::{
    ExternalPayloadOwner, ExternalPointerSlots, ExternalStorageKind, ExternalStorageValidationError,
};
use crate::layout::*;
use crate::managed_reference::{tag_of, tag_valid, untag};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};

/// Result of a Cheney copying collection, containing statistics about the collection.
pub struct CopyResult {
    pub bytes_copied: usize,
}

/// Stable descriptor owners and scratch for the prepared bump-region collector.
/// Descriptor addresses are the header identities; object addresses are found
/// afresh from the exact initialized region on every collection.
pub struct DescriptorSpace {
    static_region: Option<Arc<crate::static_region::StaticRegion>>,
    descriptors: HashMap<usize, Arc<ObjectDescriptor>>,
    object_starts: Vec<u64>,
    root_slots: Vec<usize>,
    updated_visited: Vec<u64>,
    updated_path: Vec<usize>,
    external_payloads: HashMap<usize, ExternalStorageKind>,
}

impl DescriptorSpace {
    /// Resolve a live header through this space's pinned descriptor owner.
    /// This validates metadata identity, not the provenance of an object pointer.
    pub fn live_descriptor(&self, header: usize) -> Option<&ObjectDescriptor> {
        self.descriptors.get(&header).map(Arc::as_ref)
    }

    /// Pin the layouts that may occur in a prepared heap. The reusable bitmap
    /// grows fallibly to the source region before any collection mutates it.
    pub fn new(
        descriptors: impl IntoIterator<Item = Arc<ObjectDescriptor>>,
    ) -> Result<Self, DescriptorTraceError> {
        let mut owners = HashMap::new();
        for descriptor in descriptors {
            owners
                .try_reserve(1)
                .map_err(|_| DescriptorTraceError::MetadataAllocation)?;
            owners.insert(descriptor.initial_header_word(), descriptor);
        }
        Ok(Self {
            static_region: None,
            descriptors: owners,
            object_starts: Vec::new(),
            root_slots: Vec::new(),
            updated_visited: Vec::new(),
            updated_path: Vec::new(),
            external_payloads: HashMap::new(),
        })
    }

    /// Authenticate and expand a shared payload once per copy phase. This is
    /// traversal scratch, not allocation ownership. The copier must clear it
    /// for growth recopies and for each half of promotion/fixup.
    fn expand_external(
        &mut self,
        published: *mut u8,
        kind: ExternalStorageKind,
        owner: &dyn ExternalPayloadOwner,
    ) -> Result<Option<ExternalPointerSlots>, DescriptorTraceError> {
        if let Some(previous) = self.external_payloads.get(&(published as usize)) {
            return if *previous == kind {
                Ok(None)
            } else {
                Err(DescriptorTraceError::ExternalPayload(
                    ExternalStorageValidationError::KindMismatch {
                        expected: kind,
                        actual: *previous,
                    },
                ))
            };
        }
        let slots = owner
            .slots(published, kind)
            .map_err(DescriptorTraceError::ExternalPayload)?;
        self.external_payloads
            .try_reserve(1)
            .map_err(|_| DescriptorTraceError::MetadataAllocation)?;
        self.external_payloads.insert(published as usize, kind);
        Ok(Some(slots))
    }

    /// Pin the closed immutable allocation before installing this space in a
    /// machine. Its fields cannot acquire nursery edges, so GC never scans it.
    pub fn with_static_region(mut self, region: Arc<crate::static_region::StaticRegion>) -> Self {
        self.static_region = Some(region);
        self
    }

    fn prepare_roots(&mut self, root_ptrs: &[*mut *mut u8]) -> Result<(), DescriptorTraceError> {
        self.root_slots.clear();
        self.root_slots
            .try_reserve(root_ptrs.len())
            .map_err(|_| DescriptorTraceError::MetadataAllocation)?;
        self.root_slots
            .extend(root_ptrs.iter().map(|&slot| slot as usize));
        self.root_slots.sort_unstable();
        self.root_slots.dedup();
        Ok(())
    }

    fn prepare_starts(&mut self, used: usize) -> Result<(), DescriptorTraceError> {
        let words = bitmap_words(used)?;
        if words > self.object_starts.len() {
            self.object_starts
                .try_reserve_exact(words - self.object_starts.len())
                .map_err(|_| DescriptorTraceError::MetadataAllocation)?;
            self.object_starts.resize(words, 0);
        }
        self.object_starts[..words].fill(0);
        let visited_words = words;
        if visited_words > self.updated_visited.len() {
            self.updated_visited
                .try_reserve_exact(visited_words - self.updated_visited.len())
                .map_err(|_| DescriptorTraceError::MetadataAllocation)?;
            self.updated_visited.resize(visited_words, 0);
        }
        self.updated_visited[..visited_words].fill(0);
        let max_objects = used / 16 + usize::from(used % 16 != 0);
        self.updated_path.clear();
        if max_objects > self.updated_path.capacity() {
            self.updated_path
                .try_reserve_exact(max_objects - self.updated_path.len())
                .map_err(|_| DescriptorTraceError::MetadataAllocation)?;
        }
        Ok(())
    }

    fn mark_start(&mut self, offset: usize) {
        let index = offset / 8;
        self.object_starts[index / 64] |= 1_u64 << (index % 64);
    }

    fn is_start(&self, offset: usize) -> bool {
        let index = offset / 8;
        self.object_starts[index / 64] & (1_u64 << (index % 64)) != 0
    }

    fn mark_updated(&mut self, offset: usize) -> bool {
        let index = offset / 8;
        let word = index / 64;
        let bit = 1_u64 << (index % 64);
        let was_set = self.updated_visited[word] & bit != 0;
        self.updated_visited[word] |= bit;
        was_set
    }

    fn clear_updated(&mut self, offset: usize) {
        let index = offset / 8;
        self.updated_visited[index / 64] &= !(1_u64 << (index % 64));
    }

    fn clear_updated_path(&mut self) {
        for index in 0..self.updated_path.len() {
            let address = self.updated_path[index];
            self.clear_updated(address);
        }
        self.updated_path.clear();
    }

    fn static_reference(&self, encoded: usize) -> Result<Option<usize>, DescriptorTraceError> {
        self.static_region
            .as_ref()
            .map_or(Ok(None), |region| region.admit(encoded))
    }

    /// Validate a reference against the immutable static region without
    /// admitting nursery or retained-owner addresses. Runtime promotion uses
    /// this before treating an outside-nursery result as an already-stable
    /// value.
    pub fn admit_static_reference(
        &self,
        encoded: usize,
    ) -> Result<Option<usize>, DescriptorTraceError> {
        self.static_reference(encoded)
    }

    fn root_slot_overlaps_static(&self, address: usize) -> bool {
        self.static_region.as_ref().is_some_and(|region| {
            let range = region.address_range();
            let Some(end) = address.checked_add(std::mem::size_of::<*mut u8>()) else {
                return true;
            };
            address < range.end && range.start < end
        })
    }
}

fn bitmap_words(bytes: usize) -> Result<usize, DescriptorTraceError> {
    bytes
        .checked_add(511)
        .map(|rounded| rounded / 512)
        .ok_or(DescriptorTraceError::InvalidRange)
}

/// Authenticate the complete initialized source region and reserve the
/// scratch needed by a subsequent descriptor copy.  This is deliberately a
/// separate phase from [`copy_prevalidated_descriptor_graph`]: promotion runs
/// it for every destination before the first source header is forwarded.
///
/// The source-start bitmap and all-root slot scratch remain valid after this
/// function returns.  A later selective copy may replace the active root set
/// without allocating, while retaining the authenticated source bitmap.
pub(crate) unsafe fn prepare_descriptor_copy(
    root_ptrs: &[*mut *mut u8],
    from_start: *const u8,
    from_used: usize,
    tospace: &mut [u8],
    descriptors: &mut DescriptorSpace,
) -> Result<(), DescriptorTraceError> {
    let from_base = from_start as usize;
    let from_end = from_base
        .checked_add(from_used)
        .ok_or(DescriptorTraceError::InvalidRange)?;
    let to_base = tospace.as_mut_ptr() as usize;
    let to_end = to_base
        .checked_add(tospace.len())
        .ok_or(DescriptorTraceError::InvalidRange)?;
    if from_base % 8 != 0
        || to_base % 8 != 0
        || from_used % 8 != 0
        || (from_base < to_end && to_base < from_end)
    {
        return Err(DescriptorTraceError::InvalidRange);
    }
    if tospace.len() < from_used {
        return Err(DescriptorTraceError::InsufficientSpace {
            required: from_used,
            available: tospace.len(),
        });
    }

    descriptors.prepare_roots(root_ptrs)?;
    for index in 0..descriptors.root_slots.len() {
        let address = descriptors.root_slots[index];
        let end = address
            .checked_add(std::mem::size_of::<*mut u8>())
            .ok_or(DescriptorTraceError::InvalidRange)?;
        if address == 0
            || (address < from_end && from_base < end)
            || (address < to_end && to_base < end)
            || descriptors.root_slot_overlaps_static(address)
        {
            return Err(DescriptorTraceError::InvalidRange);
        }
    }

    descriptors.prepare_starts(from_used)?;
    let mut offset = 0;
    while offset < from_used {
        let address = from_base + offset;
        let header = std::ptr::read(address as *const usize);
        let identity = header & !7;
        let extent = {
            let descriptor = descriptors
                .descriptors
                .get(&identity)
                .ok_or(DescriptorTraceError::UnknownDescriptor { address: identity })?;
            let extent = descriptor.allocation_extent() as usize;
            if extent < 16 || extent % 8 != 0 {
                return Err(DescriptorTraceError::InvalidRange);
            }
            if address % descriptor.allocation_alignment() as usize != 0 {
                return Err(DescriptorTraceError::Misaligned {
                    address,
                    alignment: descriptor.allocation_alignment(),
                });
            }
            if extent > from_used - offset {
                return Err(DescriptorTraceError::Truncated {
                    declared: descriptor.allocation_extent(),
                    available: from_used - offset,
                });
            }
            match descriptor.state(address as *const u8, extent)? {
                DescriptorState::Forwarded => return Err(DescriptorTraceError::ForwardedObject),
                DescriptorState::Live | DescriptorState::Evaluating | DescriptorState::Updated => {}
            }
            extent
        };
        descriptors.mark_start(offset);
        offset += extent;
    }
    Ok(())
}

/// Select a previously prepared root subset without reserving or allocating.
unsafe fn select_prepared_roots(
    root_ptrs: &[*mut *mut u8],
    descriptors: &mut DescriptorSpace,
) -> Result<(), DescriptorTraceError> {
    if root_ptrs.len() > descriptors.root_slots.capacity() {
        return Err(DescriptorTraceError::MetadataAllocation);
    }
    descriptors.root_slots.clear();
    descriptors
        .root_slots
        .extend(root_ptrs.iter().map(|&slot| slot as usize));
    descriptors.root_slots.sort_unstable();
    descriptors.root_slots.dedup();
    Ok(())
}

/// Copy a graph after [`prepare_descriptor_copy`] has authenticated the source
/// generation.  `admitted` is consulted for already-forwarded source objects
/// and for external old/static targets; ordinary Cheney collection passes
/// `None`, preserving its rejection of preexisting forwarding headers.
pub(crate) unsafe fn copy_prevalidated_descriptor_graph(
    root_ptrs: &[*mut *mut u8],
    from_start: *const u8,
    from_used: usize,
    tospace: &mut [u8],
    descriptors: &mut DescriptorSpace,
    admitted: Option<&dyn crate::descriptor_region::DescriptorOldSpace>,
) -> Result<CopyResult, DescriptorTraceError> {
    let from_base = from_start as usize;
    let from_end = from_base
        .checked_add(from_used)
        .ok_or(DescriptorTraceError::InvalidRange)?;
    let to_base = tospace.as_mut_ptr() as usize;
    let to_end = to_base
        .checked_add(tospace.len())
        .ok_or(DescriptorTraceError::InvalidRange)?;
    if from_base % 8 != 0
        || to_base % 8 != 0
        || from_used % 8 != 0
        || (from_base < to_end && to_base < from_end)
    {
        return Err(DescriptorTraceError::InvalidRange);
    }
    if tospace.len() < from_used {
        return Err(DescriptorTraceError::InsufficientSpace {
            required: from_used,
            available: tospace.len(),
        });
    }
    select_prepared_roots(root_ptrs, descriptors)?;

    let mut free = 0;
    for index in 0..descriptors.root_slots.len() {
        let address = descriptors.root_slots[index];
        let slot = address as *mut *mut u8;
        let value = std::ptr::read(slot).cast::<u8>() as usize;
        let relocated = evacuate_descriptor(
            value,
            from_base,
            from_end,
            to_base,
            tospace.len(),
            &mut free,
            descriptors,
            admitted,
        )?;
        std::ptr::write(slot, relocated as *mut u8);
    }
    let mut scan = 0;
    while scan < free {
        let object = (to_base + scan) as *mut u8;
        let descriptor = &*descriptor_at(object);
        let extent = descriptor.allocation_extent() as usize;
        if extent > free - scan {
            return Err(DescriptorTraceError::Truncated {
                declared: descriptor.allocation_extent(),
                available: free - scan,
            });
        }
        let mut edge_error = None;
        descriptor.for_each_trace_slot(object, extent, |slot| {
            if edge_error.is_some() {
                return;
            }
            let value = std::ptr::read(slot).cast::<u8>() as usize;
            match evacuate_descriptor(
                value,
                from_base,
                from_end,
                to_base,
                tospace.len(),
                &mut free,
                descriptors,
                admitted,
            ) {
                Ok(relocated) => std::ptr::write(slot, relocated as *mut u8),
                Err(error) => edge_error = Some(error),
            }
        })?;
        if let Some(error) = edge_error {
            return Err(error);
        }
        scan += extent;
    }
    Ok(CopyResult { bytes_copied: free })
}

/// Return the raw pinned descriptor address named by an object header. The
/// complete source walk proves every source header identity before collection
/// mutates anything; copied destination headers retain that identity, and only
/// this collector writes their Forwarded state. Callers dereference the raw
/// pointer only while the collector's descriptor owners remain alive.
///
/// # Safety
/// `object` must be an exact source start proven by the pre-copy walk or a
/// collector-established destination start.
unsafe fn descriptor_at(object: *const u8) -> *const ObjectDescriptor {
    let identity = std::ptr::read(object.cast::<usize>()) & !7;
    identity as *const ObjectDescriptor
}

/// Copy the reachable prepared graph from an exact initialized bump region.
/// This performs no reachable-graph preflight: a corrupt edge discovered after
/// relocation returns an error with source, destination and roots potentially
/// changed. On any such error, the caller must permanently retire the machine
/// and retain both semispaces and descriptor/code owners through native unwind.
/// Null managed slots are allowed; non-null slots outside the source region
/// are rejected until an external-space descriptor owner exists.
///
/// # Safety
///
/// `from_start..from_start+from_used` must be a readable and writable, fully
/// initialized bump region of prepared objects; each root slot must be a valid
/// writable pointer slot. There may be no concurrent access to either region
/// or the roots. Descriptor owners must remain alive through native unwind.
pub unsafe fn cheney_copy_descriptors(
    root_ptrs: &[*mut *mut u8],
    from_start: *const u8,
    from_used: usize,
    tospace: &mut [u8],
    descriptors: &mut DescriptorSpace,
) -> Result<CopyResult, DescriptorTraceError> {
    prepare_descriptor_copy(root_ptrs, from_start, from_used, tospace, descriptors)?;
    copy_prevalidated_descriptor_graph(root_ptrs, from_start, from_used, tospace, descriptors, None)
}

/// Descriptor collection with an invocation-owned exact-start old-space
/// admission view. Ordinary nursery collection uses this entry point so an
/// old/retained target is accepted as a stable edge while arbitrary interior
/// or forwarded pointers remain rejected.
///
/// # Safety
/// Same ownership and aliasing contract as [`cheney_copy_descriptors`]. The
/// admission owner must remain alive and immutable for the duration of the
/// copy.
pub unsafe fn cheney_copy_descriptors_with_admission(
    root_ptrs: &[*mut *mut u8],
    from_start: *const u8,
    from_used: usize,
    tospace: &mut [u8],
    descriptors: &mut DescriptorSpace,
    admitted: &dyn crate::descriptor_region::DescriptorOldSpace,
) -> Result<CopyResult, DescriptorTraceError> {
    prepare_descriptor_copy(root_ptrs, from_start, from_used, tospace, descriptors)?;
    copy_prevalidated_descriptor_graph(
        root_ptrs,
        from_start,
        from_used,
        tospace,
        descriptors,
        Some(admitted),
    )
}

/// Copy with authenticated external payload edges. A shared payload expands
/// once per copy; its slots already in the root snapshot must not be rewritten
/// again. External Address identities themselves never move or get untagged.
///
/// # Safety
/// All contracts of `cheney_copy_descriptors_with_admission` apply. The payload
/// owner must retain every external allocation through success or native
/// unwind after failure, with no mutation except this copy's slot rewrites.
pub unsafe fn cheney_copy_descriptors_with_external(
    _root_ptrs: &[*mut *mut u8],
    _from_start: *const u8,
    _from_used: usize,
    _tospace: &mut [u8],
    _descriptors: &mut DescriptorSpace,
    _admitted: Option<&dyn crate::descriptor_region::DescriptorOldSpace>,
    _external: &dyn ExternalPayloadOwner,
) -> Result<CopyResult, DescriptorTraceError> {
    // wave5:external-graph: wire the existing source authentication and Cheney
    // copier to expand_external; do not add a second copying algorithm.
    Err(DescriptorTraceError::MissingExternalOwner)
}

unsafe fn evacuate_descriptor(
    encoded: usize,
    from_base: usize,
    from_end: usize,
    to_base: usize,
    to_capacity: usize,
    free: &mut usize,
    descriptors: &mut DescriptorSpace,
    admitted: Option<&dyn crate::descriptor_region::DescriptorOldSpace>,
) -> Result<usize, DescriptorTraceError> {
    if encoded == 0 {
        return Ok(0);
    }
    let tag = tag_of(encoded);
    let address = untag(encoded);
    if address == 0 {
        return Err(DescriptorTraceError::TaggedNull { value: encoded });
    }
    if let Some(reference) = descriptors.static_reference(encoded)? {
        return Ok(reference);
    }
    if let Some(owner) = admitted {
        if let Some(reference) = owner.admit(encoded)? {
            return Ok(reference);
        }
    }
    if address < from_base || address >= from_end || (address - from_base) % 8 != 0 {
        return Err(DescriptorTraceError::InvalidManagedPointer { address });
    }
    let offset = address - from_base;
    if !descriptors.is_start(offset) {
        return Err(DescriptorTraceError::InvalidManagedPointer { address });
    }
    let pointer = address as *mut u8;
    let descriptor = &*descriptor_at(pointer);
    let state = descriptor.state(pointer, from_end - address)?;
    if state == DescriptorState::Forwarded {
        if descriptor.kind() == ObjectKind::Thunk {
            if tag != 0 {
                return Err(DescriptorTraceError::InvalidManagedTag { address, tag });
            }
        } else if !tag_valid(
            tag,
            descriptor.kind(),
            DescriptorState::Live,
            descriptor.constructor_tag(),
        ) {
            return Err(DescriptorTraceError::InvalidManagedTag { address, tag });
        }
        let stored_target = std::ptr::read(pointer.add(8).cast::<usize>());
        let probe = if descriptor.kind() == ObjectKind::Thunk {
            stored_target
        } else {
            (stored_target & !7) | usize::from(tag)
        };
        if let Some(reference) = descriptors.static_reference(probe)? {
            return Ok(reference);
        }
        if let Some(owner) = admitted {
            if let Some(reference) = owner.admit(probe)? {
                return Ok(reference);
            }
        }
        let target = forwarded_target(pointer, to_base, *free)?;
        let target_address = untag(target);
        if descriptor.kind() == ObjectKind::Thunk {
            let destination = &*descriptor_at(target_address as *const u8);
            let destination_state = destination.state(
                target_address as *const u8,
                to_base + *free - target_address,
            )?;
            if is_evaluated_kind(destination.kind()) && destination_state == DescriptorState::Live {
                let target_tag = tag_of(target);
                if !tag_valid(
                    target_tag,
                    destination.kind(),
                    destination_state,
                    destination.constructor_tag(),
                ) {
                    return Err(DescriptorTraceError::InvalidManagedTag {
                        address: target_address,
                        tag: target_tag,
                    });
                }
                return Ok(target);
            }
        }
        return Ok(target_address | usize::from(tag));
    }
    if !tag_valid(tag, descriptor.kind(), state, descriptor.constructor_tag()) {
        return Err(DescriptorTraceError::InvalidManagedTag { address, tag });
    }
    if state == DescriptorState::Updated {
        return resolve_updated(
            pointer,
            tag,
            from_base,
            from_end,
            to_base,
            to_capacity,
            free,
            descriptors,
            admitted,
        );
    }
    copy_descriptor(pointer, tag, descriptor, to_base, to_capacity, free)
}

unsafe fn copy_descriptor(
    pointer: *mut u8,
    tag: u8,
    descriptor: &ObjectDescriptor,
    to_base: usize,
    to_capacity: usize,
    free: &mut usize,
) -> Result<usize, DescriptorTraceError> {
    let extent = descriptor.allocation_extent() as usize;
    let required = free
        .checked_add(extent)
        .ok_or(DescriptorTraceError::InvalidRange)?;
    if required > to_capacity {
        return Err(DescriptorTraceError::InsufficientSpace {
            required,
            available: to_capacity,
        });
    }
    // Every source allocation and descriptor is eight-byte aligned, so a
    // sufficiently large destination needs no per-object padding.
    if (to_base + *free) % descriptor.allocation_alignment() as usize != 0 {
        return Err(DescriptorTraceError::Misaligned {
            address: to_base + *free,
            alignment: descriptor.allocation_alignment(),
        });
    }
    std::ptr::copy_nonoverlapping(pointer, (to_base + *free) as *mut u8, extent);
    descriptor.install_forwarding(pointer, (to_base + *free) as *mut u8);
    *free = required;
    Ok((to_base + required - extent) | usize::from(tag))
}

fn is_evaluated_kind(kind: ObjectKind) -> bool {
    matches!(
        kind,
        ObjectKind::Constructor | ObjectKind::Function | ObjectKind::Pap
    )
}

unsafe fn forwarded_target(
    object: *const u8,
    to_base: usize,
    free: usize,
) -> Result<usize, DescriptorTraceError> {
    let target = std::ptr::read(object.add(8).cast::<usize>());
    let target_address = untag(target);
    let target_end = target_address
        .checked_add(8)
        .ok_or(DescriptorTraceError::InvalidRange)?;
    if target_address < to_base || target_address % 8 != 0 || target_end > to_base + free {
        return Err(DescriptorTraceError::InvalidRange);
    }
    Ok(target)
}

unsafe fn resolve_updated(
    first: *mut u8,
    first_tag: u8,
    from_base: usize,
    from_end: usize,
    to_base: usize,
    to_capacity: usize,
    free: &mut usize,
    descriptors: &mut DescriptorSpace,
    admitted: Option<&dyn crate::descriptor_region::DescriptorOldSpace>,
) -> Result<usize, DescriptorTraceError> {
    descriptors.updated_path.clear();
    let mut current = first;
    let mut current_tag = first_tag;
    loop {
        let address = current as usize;
        let offset = address
            .checked_sub(from_base)
            .ok_or(DescriptorTraceError::InvalidManagedPointer { address })?;
        let descriptor = &*descriptor_at(current);
        let actual_state = descriptor.state(current, from_end - address)?;
        if actual_state == DescriptorState::Forwarded {
            if descriptor.kind() != ObjectKind::Thunk {
                if !tag_valid(
                    current_tag,
                    descriptor.kind(),
                    DescriptorState::Live,
                    descriptor.constructor_tag(),
                ) {
                    return Err(DescriptorTraceError::InvalidManagedTag {
                        address,
                        tag: current_tag,
                    });
                }
                let stored_target = std::ptr::read(current.add(8).cast::<usize>());
                if let Some(owner) = admitted {
                    if let Some(reference) =
                        owner.admit((stored_target & !7) | usize::from(current_tag))?
                    {
                        forward_updated_path(descriptors, from_base, reference);
                        return Ok(reference);
                    }
                }
                let target = forwarded_target(current, to_base, *free)?;
                let target_address = untag(target);
                let destination = &*descriptor_at(target_address as *const u8);
                let destination_state = destination.state(
                    target_address as *const u8,
                    to_base + *free - target_address,
                )?;
                if !is_evaluated_kind(destination.kind())
                    || destination_state != DescriptorState::Live
                {
                    return Err(DescriptorTraceError::InvalidUpdatedTarget);
                }
                if !tag_valid(
                    current_tag,
                    destination.kind(),
                    destination_state,
                    destination.constructor_tag(),
                ) {
                    return Err(DescriptorTraceError::InvalidManagedTag {
                        address: target_address,
                        tag: current_tag,
                    });
                }
                let relocated = target_address | usize::from(current_tag);
                forward_updated_path(descriptors, from_base, relocated);
                return Ok(relocated);
            }
            if current_tag != 0 {
                return Err(DescriptorTraceError::InvalidManagedTag {
                    address,
                    tag: current_tag,
                });
            }
            let stored_target = std::ptr::read(current.add(8).cast::<usize>());
            if let Some(reference) = descriptors.static_reference(stored_target)? {
                forward_updated_path(descriptors, from_base, reference);
                return Ok(reference);
            }
            if let Some(owner) = admitted {
                if let Some(reference) = owner.admit(stored_target)? {
                    forward_updated_path(descriptors, from_base, reference);
                    return Ok(reference);
                }
            }
            let target = forwarded_target(current, to_base, *free)?;
            let target_address = untag(target);
            let destination = &*descriptor_at(target_address as *const u8);
            let destination_state = destination.state(
                target_address as *const u8,
                to_base + *free - target_address,
            )?;
            if !is_evaluated_kind(destination.kind()) || destination_state != DescriptorState::Live
            {
                return Err(DescriptorTraceError::InvalidUpdatedTarget);
            }
            let target_tag = tag_of(target);
            if !tag_valid(
                target_tag,
                destination.kind(),
                destination_state,
                destination.constructor_tag(),
            ) {
                return Err(DescriptorTraceError::InvalidManagedTag {
                    address: target_address,
                    tag: target_tag,
                });
            }
            forward_updated_path(descriptors, from_base, target);
            return Ok(target);
        }
        if actual_state != DescriptorState::Updated {
            if actual_state != DescriptorState::Live || !is_evaluated_kind(descriptor.kind()) {
                return Err(DescriptorTraceError::InvalidUpdatedTarget);
            }
            if !tag_valid(
                current_tag,
                descriptor.kind(),
                actual_state,
                descriptor.constructor_tag(),
            ) {
                return Err(DescriptorTraceError::InvalidManagedTag {
                    address,
                    tag: current_tag,
                });
            }
            let result =
                copy_descriptor(current, current_tag, descriptor, to_base, to_capacity, free)? & !7;
            let relocated = result | usize::from(current_tag);
            forward_updated_path(descriptors, from_base, relocated);
            return Ok(relocated);
        }
        if !tag_valid(
            current_tag,
            descriptor.kind(),
            actual_state,
            descriptor.constructor_tag(),
        ) {
            return Err(DescriptorTraceError::InvalidManagedTag {
                address,
                tag: current_tag,
            });
        }
        if descriptors.mark_updated(offset) {
            return Err(DescriptorTraceError::UpdatedCycle { address });
        }
        descriptors.updated_path.push(offset);
        let target = std::ptr::read(current.add(8).cast::<usize>());
        if target == 0 {
            return Err(DescriptorTraceError::InvalidUpdatedTarget);
        }
        let target_address = untag(target);
        if let Some(reference) = descriptors.static_reference(target)? {
            forward_updated_path(descriptors, from_base, reference);
            return Ok(reference);
        }
        if let Some(owner) = admitted {
            if let Some(reference) = owner.admit(target)? {
                // Preserve the evaluated target's evidence exactly as stored
                // by the update commit; the old owner has validated it at its
                // exact allocation start.
                forward_updated_path(descriptors, from_base, reference);
                return Ok(reference);
            }
        }
        if target_address == 0
            || target_address < from_base
            || target_address >= from_end
            || (target_address - from_base) % 8 != 0
            || !descriptors.is_start(target_address - from_base)
        {
            return Err(DescriptorTraceError::InvalidManagedPointer {
                address: target_address,
            });
        }
        current = target_address as *mut u8;
        current_tag = tag_of(target);
    }
}

unsafe fn forward_updated_path(
    descriptors: &mut DescriptorSpace,
    from_base: usize,
    relocated: usize,
) {
    for index in 0..descriptors.updated_path.len() {
        let offset = descriptors.updated_path[index];
        let thunk = from_base + offset;
        let descriptor = &*descriptor_at(thunk as *const u8);
        descriptor.install_forwarding_tagged(thunk as *mut u8, relocated);
    }
    descriptors.clear_updated_path();
}

fn is_in_range(ptr: *const u8, start: *const u8, end: *const u8) -> bool {
    (ptr as usize) >= (start as usize) && (ptr as usize) < (end as usize)
}

/// Process-global test override for `checked_scanning_enabled`: 0 = unset
/// (defer to `TIDEPOOL_HEAP_VERIFY`), 1 = force on, 2 = force off.
/// `env::set_var` is racy against the `OnceLock`-cached env read below (it
/// latches the FIRST read), so tests need a way to force checked scanning
/// on or off in EITHER direction without touching the environment —
/// including forcing it off when `TIDEPOOL_HEAP_VERIFY=1` is already set.
static CHECKED_SCANNING_OVERRIDE: AtomicU8 = AtomicU8::new(0);

/// Test-only: force diagnostic checked scanning on (`true`) or off (`false`),
/// independent of `TIDEPOOL_HEAP_VERIFY`. Not part of the public API.
#[doc(hidden)]
pub fn set_checked_scanning(on: bool) {
    CHECKED_SCANNING_OVERRIDE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
}

/// Test-only: clear the override and defer back to `TIDEPOOL_HEAP_VERIFY`.
/// Not part of the public API.
#[doc(hidden)]
pub fn clear_checked_scanning_override() {
    CHECKED_SCANNING_OVERRIDE.store(0, Ordering::Relaxed);
}

/// Diagnostic mode: a size/count containment violation panics with full
/// detail instead of degrading silently. `tidepool-heap` must not depend on
/// `tidepool-codegen`, so this mirrors (rather than shares) the
/// `heap_verify_enabled` `AtomicBool`+`OnceLock` pattern in
/// `tidepool-codegen/src/host_fns/gc.rs`; codegen's `set_heap_verify` forwards
/// here so flipping one knob enables both.
fn checked_scanning_enabled() -> bool {
    match CHECKED_SCANNING_OVERRIDE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            static ON: OnceLock<bool> = OnceLock::new();
            *ON.get_or_init(|| std::env::var("TIDEPOOL_HEAP_VERIFY").is_ok_and(|v| v == "1"))
        }
    }
}

/// Handle a size/count containment violation discovered before constructing
/// raw field-slot pointers from a derived count. In diagnostic mode
/// (`checked_scanning_enabled`) this PANICS with the object's tag, size, the
/// offending relationship, and its first bytes — turning corrupted metadata
/// into a loud, diagnosable failure instead of undefined behavior during the
/// copy. In normal mode it emits an always-on stderr breadcrumb and returns,
/// leaving the caller to degrade safely (skip the object's fields, or clamp
/// its size to guarantee scan progress) rather than trust the bogus count.
///
/// `avail` is the number of bytes actually known-readable starting at `obj`
/// (the distance to the end of the containing arena/tospace/test buffer) —
/// NOT the object's own (possibly corrupted) `size` field. The diagnostic
/// dump is bounded by it, so a corrupted `size` can never make this function
/// itself read past real memory.
///
/// # Safety
///
/// `obj` must be valid for reads of at least `HEADER_SIZE` bytes (every
/// object is written with at least a tag+size header before it becomes
/// reachable, so this holds even when `size` itself is the corrupted value)
/// and `avail` must not overstate the bytes actually readable from `obj`.
unsafe fn report_violation(obj: *const u8, tag: u8, size: usize, avail: usize, what: &str) {
    // Never dump fewer than the always-valid header, never more than 32
    // bytes (`size` itself may be the corrupted value under inspection), and
    // never more than `avail` — the only bound backed by real memory extent.
    let dump_len = size.clamp(HEADER_SIZE, 32).min(avail);
    // SAFETY: obj is valid for at least HEADER_SIZE bytes per this fn's
    // safety contract; dump_len is clamped to [HEADER_SIZE, 32] and further
    // capped at avail, which the caller guarantees is real-memory-backed.
    let bytes = std::slice::from_raw_parts(obj, dump_len);
    if checked_scanning_enabled() {
        panic!(
            "[GC RAW] malformed heap object: {what}\n  tag={tag} size={size} avail={avail}\n  first {dump_len} bytes: {bytes:02x?}"
        );
    }
    eprintln!(
        "[GC RAW BUG] {what} (tag={tag} size={size} avail={avail}) — skipping rather than \
         trusting the bogus count; first {dump_len} bytes: {bytes:02x?}"
    );
}

/// `base + count.checked_mul(FIELD_STRIDE)`, or `None` on overflow.
fn field_region_end(base: usize, count: usize) -> Option<usize> {
    count
        .checked_mul(FIELD_STRIDE)
        .and_then(|span| base.checked_add(span))
}

/// Copy a single heap object from `old_ptr` to `to_base + *free` and install a
/// forwarding pointer at the old location. If the object has already been forwarded,
/// returns the new location without copying.
///
/// `avail` is the number of bytes known-readable starting at `old_ptr` (used
/// only to bound the diagnostic dump in [`report_violation`] — see its docs).
///
/// # Safety
///
/// - `old_ptr` must point to a valid, 8-byte-aligned heap object with a valid tag/size header.
/// - `to_base` must point to a buffer with enough space at offset `*free` to hold the object.
/// - The caller must ensure `old_ptr` is not inside the to-space (no aliasing).
/// - `avail` must not overstate the bytes actually readable from `old_ptr`.
unsafe fn evacuate(old_ptr: *mut u8, to_base: *mut u8, free: &mut usize, avail: usize) -> *mut u8 {
    // SAFETY: old_ptr is a valid heap object per caller's contract; tag is at offset 0.
    let tag = read_tag(old_ptr);
    if tag == TAG_FORWARDED {
        // SAFETY: Forwarded objects store the new pointer at offset 8, written by a prior evacuate call.
        return *(old_ptr.add(8) as *const *mut u8);
    }
    // SAFETY: old_ptr is a valid, non-forwarded heap object; size is at offset 1.
    let size = read_size(old_ptr) as usize;
    // A degenerate size (< the 8-byte header) can never legitimately occur —
    // clamp it to HEADER_SIZE so the copy below always makes forward
    // progress instead of risking a zero-length "copy" that leaves free
    // unchanged and corrupts subsequent Cheney-scan bookkeeping.
    let size = if size < HEADER_SIZE {
        report_violation(
            old_ptr,
            tag,
            size,
            avail,
            "size below header minimum during evacuate",
        );
        HEADER_SIZE
    } else {
        size
    };
    let aligned = size.checked_add(7).unwrap_or(size) & !7;
    // SAFETY: to_base + *free is within tospace bounds (caller guarantees sufficient capacity).
    let new_ptr = to_base.add(*free);
    // SAFETY: old_ptr and new_ptr are non-overlapping (from-space vs to-space), both valid for `aligned` bytes.
    std::ptr::copy_nonoverlapping(old_ptr, new_ptr, aligned);
    // SAFETY: Installing forwarding pointer: old object is no longer needed, we overwrite
    // tag with TAG_FORWARDED and store new_ptr at offset 8. Object is at least 8+8 bytes.
    *old_ptr = TAG_FORWARDED;
    *(old_ptr.add(8) as *mut *mut u8) = new_ptr;
    *free += aligned;
    new_ptr
}

/// Invoke a callback for each pointer field in a heap object.
///
/// The callback receives a mutable pointer to each pointer field slot within the
/// object, allowing the caller to read or update the stored pointer value.
///
/// `avail` is the number of bytes actually known-readable starting at `obj`
/// — the real extent of the containing arena/tospace/allocation, NOT the
/// object's own (possibly corrupted) `size` field. Every fixed-offset
/// metadata read (a variant's stored count, its thunk-state byte) and every
/// derived field-slot offset is checked against `avail` before it is
/// dereferenced, so a corrupted `size` or count can never make this function
/// read past real memory — it can only ever cause fields to be under-scanned
/// (reported as a violation) or, in the false-negative direction, is simply
/// not achievable: `avail` is trusted, not derived from attacker-controlled
/// data.
///
/// # Safety
///
/// `obj` must point to a valid, properly aligned heap object with a valid tag
/// and a readable `HEADER_SIZE`-byte header. `avail` must not overstate the
/// bytes actually readable from `obj` (i.e. `obj .. obj + avail` must be
/// valid for reads). The object must be located in memory such that every
/// pointer field this function decides to visit (per the bounds check above)
/// is initialized and safe to read and write through the provided
/// `*mut *mut u8` pointers.
pub unsafe fn for_each_pointer_field(obj: *mut u8, avail: usize, mut f: impl FnMut(*mut *mut u8)) {
    // SAFETY: obj is a valid heap object per caller's contract; tag and size are in the header.
    let tag = read_tag(obj);
    let size = read_size(obj) as usize;
    // Always-on, one-compare containment floor: every object carries at
    // least a written tag+size header, so a smaller size is corrupted
    // metadata, not a legal shape — reject before trusting any derived
    // offset below.
    if size < HEADER_SIZE {
        report_violation(obj, tag, size, avail, "size below header minimum");
        return;
    }
    match tag {
        TAG_CLOSURE => {
            // Closure layout: num_captured at CLOSURE_NUM_CAPTURED_OFFSET,
            // followed by n pointer-sized capture slots starting at
            // CLOSURE_CAPTURED_OFFSET. The count field itself must be
            // validated (against BOTH the declared size and the real
            // readable extent) before it is dereferenced — a corrupted
            // `size` must not let this read past `avail`.
            let meta_end = CLOSURE_NUM_CAPTURED_OFFSET + 2;
            if size < meta_end || avail < meta_end {
                report_violation(
                    obj,
                    tag,
                    size,
                    avail,
                    &format!(
                        "Closure num_captured field at offset {CLOSURE_NUM_CAPTURED_OFFSET} needs {meta_end} bytes but size={size} avail={avail}"
                    ),
                );
                return;
            }
            // SAFETY: size and avail both cover [0, meta_end) per the check above.
            let n = *(obj.add(CLOSURE_NUM_CAPTURED_OFFSET) as *const u16) as usize;
            match field_region_end(CLOSURE_CAPTURED_OFFSET, n) {
                Some(needed) if needed <= size && needed <= avail => {
                    for i in 0..n {
                        f(obj.add(CLOSURE_CAPTURED_OFFSET + i * FIELD_STRIDE) as *mut *mut u8);
                    }
                }
                Some(needed) if needed > size => report_violation(
                    obj,
                    tag,
                    size,
                    avail,
                    &format!(
                        "Closure num_captured={n} needs {needed} bytes but object size is {size}"
                    ),
                ),
                Some(needed) => report_violation(
                    obj,
                    tag,
                    size,
                    avail,
                    &format!(
                        "Closure num_captured={n} needs {needed} bytes but only {avail} bytes are readable"
                    ),
                ),
                None => report_violation(
                    obj,
                    tag,
                    size,
                    avail,
                    &format!(
                        "Closure num_captured={n} overflows the capture-region size computation"
                    ),
                ),
            }
        }
        TAG_CON => {
            // Con layout: num_fields at CON_NUM_FIELDS_OFFSET, followed by n
            // pointer-sized field slots starting at CON_FIELDS_OFFSET. Same
            // pre-dereference validation as Closure above.
            let meta_end = CON_NUM_FIELDS_OFFSET + 2;
            if size < meta_end || avail < meta_end {
                report_violation(
                    obj,
                    tag,
                    size,
                    avail,
                    &format!(
                        "Con num_fields field at offset {CON_NUM_FIELDS_OFFSET} needs {meta_end} bytes but size={size} avail={avail}"
                    ),
                );
                return;
            }
            // SAFETY: size and avail both cover [0, meta_end) per the check above.
            let n = *(obj.add(CON_NUM_FIELDS_OFFSET) as *const u16) as usize;
            match field_region_end(CON_FIELDS_OFFSET, n) {
                Some(needed) if needed <= size && needed <= avail => {
                    for i in 0..n {
                        f(obj.add(CON_FIELDS_OFFSET + i * FIELD_STRIDE) as *mut *mut u8);
                    }
                }
                Some(needed) if needed > size => report_violation(
                    obj,
                    tag,
                    size,
                    avail,
                    &format!("Con num_fields={n} needs {needed} bytes but object size is {size}"),
                ),
                Some(needed) => report_violation(
                    obj,
                    tag,
                    size,
                    avail,
                    &format!(
                        "Con num_fields={n} needs {needed} bytes but only {avail} bytes are readable"
                    ),
                ),
                None => report_violation(
                    obj,
                    tag,
                    size,
                    avail,
                    &format!("Con num_fields={n} overflows the field-region size computation"),
                ),
            }
        }
        TAG_THUNK => {
            // Every thunk state shares ONE canonical minimum size —
            // THUNK_MIN_SIZE (header + state byte + code-ptr/indirection
            // slot) — matching layout.rs's ABI. This must be checked before
            // the state byte itself is read (THUNK_STATE_OFFSET lies inside
            // this minimum) and before any capture count is derived from
            // `size`, so the collector and the layout ABI can never bless
            // different minimum shapes.
            if size < THUNK_MIN_SIZE {
                report_violation(
                    obj,
                    tag,
                    size,
                    avail,
                    &format!("Thunk size {size} < THUNK_MIN_SIZE ({THUNK_MIN_SIZE})"),
                );
                return;
            }
            if avail < THUNK_STATE_OFFSET + 1 {
                report_violation(
                    obj,
                    tag,
                    size,
                    avail,
                    &format!(
                        "Thunk state byte at offset {THUNK_STATE_OFFSET} needs {} bytes but only {avail} are readable",
                        THUNK_STATE_OFFSET + 1
                    ),
                );
                return;
            }
            // SAFETY: avail covers THUNK_STATE_OFFSET per the check above.
            let state = *obj.add(THUNK_STATE_OFFSET);
            match state {
                // BLACKHOLE = mid-evaluation: the thunk's code may still read
                // its capture slots AFTER a GC its own allocations triggered,
                // so captures must be evacuated and the slots updated exactly
                // like an unevaluated thunk's — otherwise a resumed blackhole
                // holds stale from-space pointers.
                THUNK_UNEVALUATED | THUNK_BLACKHOLE => {
                    // Thunk captures are pointer slots from
                    // THUNK_CAPTURED_OFFSET to end of object (determined by
                    // size). Unlike Con/Closure there is no SEPARATE stored
                    // count to disagree with `size` here — the capture count
                    // is derived FROM size (already >= THUNK_MIN_SIZE ==
                    // THUNK_CAPTURED_OFFSET, so the subtraction below never
                    // underflows) — but the derived region must still be
                    // checked against `avail` before any slot is visited.
                    let n = (size - THUNK_CAPTURED_OFFSET) / FIELD_STRIDE;
                    match field_region_end(THUNK_CAPTURED_OFFSET, n) {
                        Some(needed) if needed <= avail => {
                            for i in 0..n {
                                f(obj.add(THUNK_CAPTURED_OFFSET + i * FIELD_STRIDE)
                                    as *mut *mut u8);
                            }
                        }
                        Some(needed) => report_violation(
                            obj,
                            tag,
                            size,
                            avail,
                            &format!(
                                "Thunk captures (n={n}) need {needed} bytes but only {avail} are readable"
                            ),
                        ),
                        None => report_violation(
                            obj,
                            tag,
                            size,
                            avail,
                            &format!(
                                "Thunk captures n={n} overflows the capture-region size computation"
                            ),
                        ),
                    }
                }
                THUNK_EVALUATED => {
                    // Evaluated thunk stores indirection pointer at
                    // THUNK_INDIRECTION_OFFSET. `size` is already known >=
                    // THUNK_MIN_SIZE == THUNK_INDIRECTION_OFFSET +
                    // FIELD_STRIDE, so only `avail` can still fall short.
                    let needed = THUNK_INDIRECTION_OFFSET + FIELD_STRIDE;
                    if avail >= needed {
                        f(obj.add(THUNK_INDIRECTION_OFFSET) as *mut *mut u8);
                    } else {
                        report_violation(
                            obj,
                            tag,
                            size,
                            avail,
                            &format!(
                                "Thunk (Evaluated) needs {needed} bytes but only {avail} are readable"
                            ),
                        );
                    }
                }
                // Invalid states are left untouched here; the post-GC verifier
                // (TIDEPOOL_HEAP_VERIFY=1) flags them loudly.
                _ => {}
            }
        }
        TAG_LIT => {
            // SAFETY: Lit layout: lit_tag byte at LIT_TAG_OFFSET, value field
            // at LIT_VALUE_OFFSET — reading either requires size and avail
            // both >= LIT_SIZE.
            if size < LIT_SIZE || avail < LIT_SIZE {
                report_violation(
                    obj,
                    tag,
                    size,
                    avail,
                    &format!("Lit size {size} < LIT_SIZE ({LIT_SIZE}) or avail {avail} too small"),
                );
            } else {
                // For SmallArray#/Array#, the value field holds a pointer to
                // a malloc'd, GC-external payload buffer
                // `[u64 len][ptr0..ptrN]` (`runtime_new_boxed_array`) — the
                // buffer itself is stable and never evacuated (it isn't a
                // from-space heap object), but its SLOT CONTENTS are ordinary
                // heap pointers and must be visible to the collector exactly
                // like a Con field or Closure capture. `cheney_copy`'s
                // from-space range check makes tracing these slots safe even
                // though the buffer lives outside both semispaces.
                let lit_tag = *obj.add(LIT_TAG_OFFSET);
                if lit_tag == LitTag::SmallArray as u8 || lit_tag == LitTag::Array as u8 {
                    // SAFETY: value field is a valid pointer into a live
                    // `runtime_new_boxed_array` allocation per the caller's
                    // contract on `obj`.
                    let payload = *(obj.add(LIT_VALUE_OFFSET) as *const *mut u8);
                    if !payload.is_null() {
                        // SAFETY: payload's first 8 bytes are the array length,
                        // written by `runtime_new_boxed_array` before the array
                        // becomes reachable.
                        let len = *(payload as *const u64) as usize;
                        // The malloc'd payload carries no recorded capacity
                        // field to cross-check `len` against — only the
                        // length prefix itself — so unlike Con/Closure there
                        // is no second source of truth to validate against.
                        // Array lengths are also legitimately
                        // user-controlled and can be large by design, so an
                        // invented magic cap would just produce false
                        // positives on big-but-real arrays. The one
                        // containment invariant enforceable without a second
                        // source of truth is that the derived byte span must
                        // not overflow pointer-sized arithmetic.
                        match len.checked_mul(FIELD_STRIDE).and_then(|s| s.checked_add(8)) {
                            Some(_) => {
                                for i in 0..len {
                                    f(payload.add(8 + i * FIELD_STRIDE) as *mut *mut u8);
                                }
                            }
                            None => report_violation(
                                obj,
                                tag,
                                size,
                                avail,
                                &format!(
                                    "boxed array len {len} overflows its byte-span computation (8 + len*{FIELD_STRIDE})"
                                ),
                            ),
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

/// One pointer-field slot within a heap object, as found by [`inspect_object`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldSlot {
    /// Byte offset of the slot from the object's own start (`obj`, not the
    /// containing arena) — valid for `Closure`/`Con`/`Thunk` slots, which
    /// this covers; a `Lit`'s boxed-array elements live in a separate
    /// malloc'd payload and are NOT represented here (see `inspect_object`'s
    /// doc).
    pub offset: usize,
    /// Human-readable field label, derived from the object's own tag —
    /// "Con field", "Closure capture", "Thunk field".
    pub label: &'static str,
}

fn field_label(tag: u8) -> &'static str {
    match tag {
        TAG_CON => "Con field",
        TAG_CLOSURE => "Closure capture",
        TAG_THUNK => "Thunk field",
        _ => "field",
    }
}

/// The validated, bounds-checked list of pointer-field slots in `obj` —
/// [`for_each_pointer_field`]'s own decode, collected and labeled instead of
/// streamed through a callback, for a caller (post-GC verification, debug
/// validation) that wants to inspect the shape rather than mutate the heap
/// in place. Reuses `for_each_pointer_field` verbatim (same containment
/// checks, same `report_violation` on a corrupted count) — this does not
/// reimplement or relax any of its bounds checking.
///
/// Covers `Closure` captures, `Con` fields, and `Thunk` fields (whichever
/// shape `state` selects) — every slot `for_each_pointer_field` would visit
/// AND whose address falls within `obj`'s own bytes. A `Lit` holding a
/// `SmallArray#`/`Array#` visits slots inside a SEPARATE malloc'd payload
/// buffer, which cannot be expressed as an offset from `obj`; callers that
/// need those keep using [`for_each_pointer_field`] directly (as
/// `verify_heap_post_gc` and `cheney_copy` both still do).
///
/// # Safety
/// Same contract as [`for_each_pointer_field`].
pub unsafe fn inspect_object(obj: *mut u8, avail: usize) -> Vec<FieldSlot> {
    // SAFETY: obj is a valid heap object per this function's own contract,
    // which mirrors for_each_pointer_field's.
    let tag = read_tag(obj);
    if tag == TAG_LIT {
        // A Lit's only pointer-bearing shape (SmallArray#/Array#) visits
        // slots inside a separate malloc'd payload buffer, at an address
        // with no fixed relationship to `obj` — not representable as an
        // offset from `obj` at all, so this deliberately visits nothing
        // rather than guess. See this fn's doc.
        return Vec::new();
    }
    let label = field_label(tag);
    let base = obj as usize;
    let mut slots = Vec::new();
    for_each_pointer_field(obj, avail, |slot| {
        let addr = slot as usize;
        debug_assert!(
            addr >= base,
            "inspect_object: field slot {addr:#x} precedes object base {base:#x}"
        );
        slots.push(FieldSlot {
            offset: addr - base,
            label,
        });
    });
    slots
}

/// Perform a Cheney semi-space copying garbage collection.
///
/// Scans a slice of root pointers, evacuating any live objects from the `from`
/// space (defined by `from_start` and `from_end`) into `tospace`. Root pointers
/// and any internal pointers within the copied objects are updated to point to
/// the new locations in `tospace`.
///
/// # Safety
///
/// - `root_ptrs` must be a valid slice of valid mutable slots containing pointers.
/// - `from_start` and `from_end` must define a valid memory range.
/// - `tospace` must be disjoint from the from-space range and must have sufficient
///   capacity to hold all live objects reachable from the provided roots. Exceeding
///   the capacity of `tospace` will result in out-of-bounds writes.
pub unsafe fn cheney_copy(
    root_ptrs: &[*mut *mut u8],
    from_start: *const u8,
    from_end: *const u8,
    tospace: &mut [u8],
) -> CopyResult {
    cheney_copy_impl(root_ptrs, from_start, from_end, tospace)
}

unsafe fn cheney_copy_impl(
    root_ptrs: &[*mut *mut u8],
    from_start: *const u8,
    from_end: *const u8,
    tospace: &mut [u8],
) -> CopyResult {
    let to_base = tospace.as_mut_ptr();
    let to_len = tospace.len();
    let mut free = 0usize;

    for &root_slot in root_ptrs {
        let old_ptr = *root_slot;
        if !old_ptr.is_null() && is_in_range(old_ptr, from_start, from_end) {
            let available = from_end as usize - old_ptr as usize;
            let new_ptr = evacuate(old_ptr, to_base, &mut free, available);
            *root_slot = new_ptr;
        }
    }

    let mut scan = 0usize;
    while scan < free {
        let object = to_base.add(scan);
        let available = to_len - scan;
        let object_tag = read_tag(object);
        let object_size = read_size(object) as usize;
        let object_size = if object_size < HEADER_SIZE {
            report_violation(
                object,
                object_tag,
                object_size,
                available,
                "size below header minimum during Cheney scan",
            );
            HEADER_SIZE
        } else {
            object_size
        };
        let aligned = object_size.checked_add(7).unwrap_or(object_size) & !7;

        let mut visit = |field_slot: *mut *mut u8| {
            let field_value = *field_slot;
            if !field_value.is_null() && is_in_range(field_value, from_start, from_end) {
                let field_available = from_end as usize - field_value as usize;
                let new_ptr = evacuate(field_value, to_base, &mut free, field_available);
                *field_slot = new_ptr;
            }
        };
        for_each_pointer_field(object, available, &mut visit);
        scan += aligned;
    }

    CopyResult { bytes_copied: free }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[repr(align(8))]
    struct AlignedBuf([u8; 1024]);

    unsafe fn write_lit(buf: &mut [u8], offset: usize, value: i64) -> usize {
        // SAFETY: buf is a 1024-byte aligned buffer; offset is managed by the caller
        // to ensure non-overlapping object placement. LIT_SIZE (24) fits within remaining space.
        let ptr = buf.as_mut_ptr().add(offset);
        write_header(ptr, TAG_LIT, LIT_SIZE as u32);
        *ptr.add(LIT_TAG_OFFSET) = LitTag::Int as u8;
        *(ptr.add(LIT_VALUE_OFFSET) as *mut i64) = value;
        offset + LIT_SIZE
    }

    unsafe fn write_con(buf: &mut [u8], offset: usize, con_tag: u64, fields: &[*mut u8]) -> usize {
        // SAFETY: buf is a 1024-byte aligned buffer; offset ensures non-overlapping placement.
        // The computed size fits within the buffer for small field counts used in tests.
        let ptr = buf.as_mut_ptr().add(offset);
        let size = (CON_FIELDS_OFFSET + fields.len() * FIELD_STRIDE) as u32;
        let aligned = (size as usize)
            .checked_add(7)
            .expect("heap object size too large to align")
            & !7;
        write_header(ptr, TAG_CON, size);
        *(ptr.add(CON_TAG_OFFSET) as *mut u64) = con_tag;
        *(ptr.add(CON_NUM_FIELDS_OFFSET) as *mut u16) = fields.len() as u16;
        for (i, &f) in fields.iter().enumerate() {
            *(ptr.add(CON_FIELDS_OFFSET + i * FIELD_STRIDE) as *mut *mut u8) = f;
        }
        offset + aligned
    }

    unsafe fn write_closure(
        buf: &mut [u8],
        offset: usize,
        code_ptr: *const u8,
        captures: &[*mut u8],
    ) -> usize {
        // SAFETY: buf is a 1024-byte aligned buffer; offset ensures non-overlapping placement.
        // The computed size fits within the buffer for small capture counts used in tests.
        let ptr = buf.as_mut_ptr().add(offset);
        let size = (CLOSURE_CAPTURED_OFFSET + captures.len() * FIELD_STRIDE) as u32;
        let aligned = (size as usize)
            .checked_add(7)
            .expect("heap object size too large to align")
            & !7;
        write_header(ptr, TAG_CLOSURE, size);
        *(ptr.add(CLOSURE_CODE_PTR_OFFSET) as *mut *const u8) = code_ptr;
        *(ptr.add(CLOSURE_NUM_CAPTURED_OFFSET) as *mut u16) = captures.len() as u16;
        for (i, &c) in captures.iter().enumerate() {
            *(ptr.add(CLOSURE_CAPTURED_OFFSET + i * FIELD_STRIDE) as *mut *mut u8) = c;
        }
        offset + aligned
    }

    // 1. test_copy_single_lit
    #[test]
    fn test_copy_single_lit() {
        let mut from_buf = AlignedBuf([0u8; 1024]);
        let mut to_buf = AlignedBuf([0u8; 1024]);
        let from = &mut from_buf.0;
        let to = &mut to_buf.0;
        // SAFETY: Test-only. Buffers are 8-byte aligned (repr(align(8))) and 1024 bytes,
        // sufficient for the heap objects written. Root pointers reference valid from-space objects.
        unsafe {
            let _offset = write_lit(from, 0, 42);
            let mut root = from.as_mut_ptr();
            let roots = [&mut root as *mut *mut u8];
            let res = cheney_copy(&roots, from.as_ptr(), from.as_ptr().add(1024), to);
            assert_eq!(res.bytes_copied, LIT_SIZE);
            assert_eq!(root, to.as_mut_ptr());
            assert_eq!(read_tag(root), TAG_LIT);
            assert_eq!(*(root.add(LIT_VALUE_OFFSET) as *const i64), 42);
        }
    }

    // 2. test_copy_con_with_lit_fields
    #[test]
    fn test_copy_con_with_lit_fields() {
        let mut from_buf = AlignedBuf([0u8; 1024]);
        let mut to_buf = AlignedBuf([0u8; 1024]);
        let from = &mut from_buf.0;
        let to = &mut to_buf.0;
        // SAFETY: Test-only. Aligned buffers with sufficient capacity. Con fields point
        // to valid Lit objects within from-space; cheney_copy evacuates the transitive closure.
        unsafe {
            let off1 = write_lit(from, 0, 10);
            let off2 = write_lit(from, off1, 20);
            let lit1 = from.as_mut_ptr();
            let lit2 = from.as_mut_ptr().add(off1);
            let _off3 = write_con(from, off2, 99, &[lit1, lit2]);
            let mut root = from.as_mut_ptr().add(off2);
            let roots = [&mut root as *mut *mut u8];
            let _res = cheney_copy(&roots, from.as_ptr(), from.as_ptr().add(1024), to);

            assert_eq!(root, to.as_mut_ptr()); // con is copied first
            assert_eq!(read_tag(root), TAG_CON);
            let n_fields = *(root.add(CON_NUM_FIELDS_OFFSET) as *const u16);
            assert_eq!(n_fields, 2);

            let f1 = *(root.add(CON_FIELDS_OFFSET) as *const *mut u8);
            let f2 = *(root.add(CON_FIELDS_OFFSET + FIELD_STRIDE) as *const *mut u8);
            assert_eq!(read_tag(f1), TAG_LIT);
            assert_eq!(read_tag(f2), TAG_LIT);
            assert_eq!(*(f1.add(LIT_VALUE_OFFSET) as *const i64), 10);
            assert_eq!(*(f2.add(LIT_VALUE_OFFSET) as *const i64), 20);
        }
    }

    // 3. test_copy_closure_with_captures
    #[test]
    fn test_copy_closure_with_captures() {
        let mut from_buf = AlignedBuf([0u8; 1024]);
        let mut to_buf = AlignedBuf([0u8; 1024]);
        let from = &mut from_buf.0;
        let to = &mut to_buf.0;
        // SAFETY: Test-only. Aligned buffers with sufficient capacity. Closure captures
        // a valid Lit object in from-space; code_ptr is a synthetic non-null address (not dereferenced).
        unsafe {
            let off1 = write_lit(from, 0, 100);
            let lit = from.as_mut_ptr();
            let code_ptr = 0x12345678usize as *const u8;
            let _off2 = write_closure(from, off1, code_ptr, &[lit]);
            let mut root = from.as_mut_ptr().add(off1);
            let roots = [&mut root as *mut *mut u8];
            let _res = cheney_copy(&roots, from.as_ptr(), from.as_ptr().add(1024), to);

            assert_eq!(root, to.as_mut_ptr());
            assert_eq!(read_tag(root), TAG_CLOSURE);
            assert_eq!(
                *(root.add(CLOSURE_CODE_PTR_OFFSET) as *const *const u8),
                code_ptr
            );

            let cap = *(root.add(CLOSURE_CAPTURED_OFFSET) as *const *mut u8);
            assert_eq!(read_tag(cap), TAG_LIT);
            assert_eq!(*(cap.add(LIT_VALUE_OFFSET) as *const i64), 100);
        }
    }

    // 4. test_transitive_chain
    #[test]
    fn test_transitive_chain() {
        let mut from_buf = AlignedBuf([0u8; 1024]);
        let mut to_buf = AlignedBuf([0u8; 1024]);
        let from = &mut from_buf.0;
        let to = &mut to_buf.0;
        // SAFETY: Test-only. Aligned buffers with sufficient capacity. Builds a Con->Con->Lit
        // chain; cheney_copy transitively evacuates all reachable objects.
        unsafe {
            let off1 = write_lit(from, 0, 7);
            let lit = from.as_mut_ptr();
            let off2 = write_con(from, off1, 1, &[lit]);
            let con1 = from.as_mut_ptr().add(off1);
            let _off3 = write_con(from, off2, 2, &[con1]);

            let mut root = from.as_mut_ptr().add(off2);
            let roots = [&mut root as *mut *mut u8];
            let _res = cheney_copy(&roots, from.as_ptr(), from.as_ptr().add(1024), to);

            assert_eq!(root, to.as_mut_ptr());
            assert_eq!(read_tag(root), TAG_CON);
            assert_eq!(*(root.add(CON_TAG_OFFSET) as *const u64), 2);

            let c1 = *(root.add(CON_FIELDS_OFFSET) as *const *mut u8);
            assert_eq!(read_tag(c1), TAG_CON);
            assert_eq!(*(c1.add(CON_TAG_OFFSET) as *const u64), 1);

            let l1 = *(c1.add(CON_FIELDS_OFFSET) as *const *mut u8);
            assert_eq!(read_tag(l1), TAG_LIT);
            assert_eq!(*(l1.add(LIT_VALUE_OFFSET) as *const i64), 7);
        }
    }

    // 5. test_external_pointers_unchanged
    #[test]
    fn test_external_pointers_unchanged() {
        let mut from_buf = AlignedBuf([0u8; 1024]);
        let mut to_buf = AlignedBuf([0u8; 1024]);
        let from = &mut from_buf.0;
        let to = &mut to_buf.0;
        // SAFETY: Test-only. Aligned buffers with sufficient capacity. ext_ptr is a synthetic
        // address outside from-space; GC must preserve it without dereferencing or evacuating.
        unsafe {
            let ext_ptr = 0x8899aabbccusize as *mut u8; // outside from_start..from_end
            let _off1 = write_closure(from, 0, 0x112233usize as *const u8, &[ext_ptr]);
            let mut root = from.as_mut_ptr();
            let roots = [&mut root as *mut *mut u8];
            let _res = cheney_copy(&roots, from.as_ptr(), from.as_ptr().add(1024), to);

            assert_eq!(root, to.as_mut_ptr());
            let cap = *(root.add(CLOSURE_CAPTURED_OFFSET) as *const *mut u8);
            assert_eq!(cap, ext_ptr); // remains unchanged
        }
    }

    // 6. test_diamond_sharing
    #[test]
    fn test_diamond_sharing() {
        let mut from_buf = AlignedBuf([0u8; 1024]);
        let mut to_buf = AlignedBuf([0u8; 1024]);
        let from = &mut from_buf.0;
        let to = &mut to_buf.0;
        // SAFETY: Test-only. Aligned buffers with sufficient capacity. Two roots point to the
        // same Lit object; forwarding pointers ensure it is copied exactly once.
        unsafe {
            let _off1 = write_lit(from, 0, 42);
            let lit = from.as_mut_ptr();
            let mut root1 = lit;
            let mut root2 = lit;
            let roots = [&mut root1 as *mut *mut u8, &mut root2 as *mut *mut u8];

            let res = cheney_copy(&roots, from.as_ptr(), from.as_ptr().add(1024), to);

            assert_eq!(res.bytes_copied, LIT_SIZE); // copied only once
            assert_eq!(root1, root2);
            assert_eq!(root1, to.as_mut_ptr());
        }
    }

    // 7. test_dead_objects_not_copied
    #[test]
    fn test_dead_objects_not_copied() {
        let mut from_buf = AlignedBuf([0u8; 1024]);
        let mut to_buf = AlignedBuf([0u8; 1024]);
        let from = &mut from_buf.0;
        let to = &mut to_buf.0;
        // SAFETY: Test-only. Aligned buffers with sufficient capacity. Three Lits written
        // but only one is rooted; unreachable objects must not be copied.
        unsafe {
            let off1 = write_lit(from, 0, 1);
            let off2 = write_lit(from, off1, 2); // this one is rooted
            let _off3 = write_lit(from, off2, 3);

            let mut root = from.as_mut_ptr().add(off1);
            let roots = [&mut root as *mut *mut u8];

            let res = cheney_copy(&roots, from.as_ptr(), from.as_ptr().add(1024), to);

            assert_eq!(res.bytes_copied, LIT_SIZE); // only 1 copied
            assert_eq!(read_tag(root), TAG_LIT);
            assert_eq!(*(root.add(LIT_VALUE_OFFSET) as *const i64), 2);
        }
    }

    // 8. test_for_each_pointer_field_lit
    #[test]
    fn test_for_each_pointer_field_lit() {
        let mut buf_data = AlignedBuf([0u8; 1024]);
        let buf = &mut buf_data.0;
        // SAFETY: Test-only. Aligned buffer contains a valid Lit object. Lits have no pointer fields.
        unsafe {
            write_lit(buf, 0, 10);
            let mut count = 0;
            for_each_pointer_field(buf.as_mut_ptr(), 1024, |_| {
                count += 1;
            });
            assert_eq!(count, 0);
        }
    }

    // 9. test_for_each_pointer_field_con
    #[test]
    fn test_for_each_pointer_field_con() {
        let mut buf_data = AlignedBuf([0u8; 1024]);
        let buf = &mut buf_data.0;
        // SAFETY: Test-only. Aligned buffer contains a valid Con with 2 synthetic pointer fields.
        unsafe {
            write_con(buf, 0, 1, &[0x1000 as *mut u8, 0x2000 as *mut u8]);
            let mut ptrs = Vec::new();
            for_each_pointer_field(buf.as_mut_ptr(), 1024, |p| {
                ptrs.push(*p);
            });
            assert_eq!(ptrs, vec![0x1000 as *mut u8, 0x2000 as *mut u8]);
        }
    }

    // 10. test_for_each_pointer_field_closure
    #[test]
    fn test_for_each_pointer_field_closure() {
        let mut buf_data = AlignedBuf([0u8; 1024]);
        let buf = &mut buf_data.0;
        // SAFETY: Test-only. Aligned buffer contains a valid Closure with 1 synthetic capture pointer.
        unsafe {
            write_closure(buf, 0, 0x9999 as *const u8, &[0x3000 as *mut u8]);
            let mut ptrs = Vec::new();
            for_each_pointer_field(buf.as_mut_ptr(), 1024, |p| {
                ptrs.push(*p);
            });
            assert_eq!(ptrs, vec![0x3000 as *mut u8]); // code_ptr is excluded
        }
    }

    // 11b. test_inspect_object_con — offsets/labels match for_each_pointer_field
    #[test]
    fn test_inspect_object_con() {
        let mut buf_data = AlignedBuf([0u8; 1024]);
        let buf = &mut buf_data.0;
        // SAFETY: Test-only. Aligned buffer contains a valid Con with 2 synthetic pointer fields.
        unsafe {
            write_con(buf, 0, 1, &[0x1000 as *mut u8, 0x2000 as *mut u8]);
            let slots = inspect_object(buf.as_mut_ptr(), 1024);
            assert_eq!(
                slots,
                vec![
                    FieldSlot {
                        offset: CON_FIELDS_OFFSET,
                        label: "Con field"
                    },
                    FieldSlot {
                        offset: CON_FIELDS_OFFSET + FIELD_STRIDE,
                        label: "Con field"
                    },
                ]
            );
        }
    }

    // 11c. test_inspect_object_lit_visits_nothing — array payload isn't
    // obj-relative, so inspect_object deliberately returns empty rather than
    // guess at an offset.
    #[test]
    fn test_inspect_object_lit_visits_nothing() {
        let mut buf_data = AlignedBuf([0u8; 1024]);
        let buf = &mut buf_data.0;
        // SAFETY: Test-only. Aligned buffer contains a valid Lit.
        unsafe {
            write_lit(buf, 0, 42);
            let slots = inspect_object(buf.as_mut_ptr(), 1024);
            assert!(slots.is_empty());
        }
    }

    // 11. test_for_each_pointer_field_thunk_blackhole
    #[test]
    fn test_for_each_pointer_field_thunk_blackhole() {
        let mut buf_data = AlignedBuf([0u8; 1024]);
        let buf = &mut buf_data.0;
        // SAFETY: Test-only. Aligned buffer contains a valid Thunk in BlackHole state (no pointer fields).
        unsafe {
            let ptr = buf.as_mut_ptr();
            // Minimum-sized blackhole (size == THUNK_MIN_SIZE, 24): no
            // capture region, no visits.
            write_header(ptr, TAG_THUNK, THUNK_MIN_SIZE as u32);
            *ptr.add(THUNK_STATE_OFFSET) = THUNK_BLACKHOLE;
            let mut count = 0;
            for_each_pointer_field(ptr, 1024, |_| {
                count += 1;
            });
            assert_eq!(count, 0, "minimum-sized blackhole has no capture region");

            // A blackhole WITH captures must have them visited exactly like
            // an unevaluated thunk — its code may still read the slots after
            // a GC it triggered itself.
            let ptr2 = buf.as_mut_ptr().add(64);
            write_header(ptr2, TAG_THUNK, 24 + 8 * 3);
            *ptr2.add(THUNK_STATE_OFFSET) = THUNK_BLACKHOLE;
            let mut count2 = 0;
            for_each_pointer_field(ptr2, 1024 - 64, |_| {
                count2 += 1;
            });
            assert_eq!(count2, 3, "blackhole captures must be visible to GC (C6)");
        }
    }
}

#[cfg(test)]
mod descriptor_copy_tests {
    use super::*;
    use crate::descriptor_region::DescriptorArena;
    use crate::execution_descriptor::{DescriptorState, ObjectKind};
    use crate::static_region::{StaticImage, StaticRelocation};
    use std::collections::BTreeMap;
    use tidepool_repr::execution_schema::{
        Architecture, Endianness, RuntimeRep, StorageLayout, TargetDescriptor,
    };

    fn descriptor(kind: ObjectKind, reps: &[RuntimeRep]) -> Arc<ObjectDescriptor> {
        let target = TargetDescriptor {
            architecture: Architecture::X86_64,
            endianness: Endianness::Little,
            pointer_width: 64,
            word_width: 64,
            abi: "system-v".into(),
            features: Vec::new(),
        };
        let layout = StorageLayout::for_reps(&target, reps).unwrap();
        Arc::new(
            match kind {
                ObjectKind::Constructor => ObjectDescriptor::constructor(1, layout, None),
                _ => ObjectDescriptor::new(kind, layout, None),
            }
            .unwrap(),
        )
    }

    fn constructor_descriptor(tag: u32, reps: &[RuntimeRep]) -> Arc<ObjectDescriptor> {
        let target = TargetDescriptor {
            architecture: Architecture::X86_64,
            endianness: Endianness::Little,
            pointer_width: 64,
            word_width: 64,
            abi: "system-v".into(),
            features: Vec::new(),
        };
        Arc::new(
            ObjectDescriptor::constructor(
                tag,
                StorageLayout::for_reps(&target, reps).unwrap(),
                None,
            )
            .unwrap(),
        )
    }

    fn static_cycle() -> Arc<crate::static_region::StaticRegion> {
        let first = constructor_descriptor(1, &[RuntimeRep::LiftedRef]);
        let second = constructor_descriptor(2, &[RuntimeRep::LiftedRef]);
        let mut words = vec![0_u64; 4];
        words[0] = first.initial_header_word() as u64;
        words[2] = second.initial_header_word() as u64;
        let image = StaticImage::new(
            words,
            vec![
                StaticRelocation {
                    slot_offset: 8,
                    target_offset: 16,
                    tag: second.tag(),
                },
                StaticRelocation {
                    slot_offset: 24,
                    target_offset: 0,
                    tag: first.tag(),
                },
            ],
            BTreeMap::from([(tidepool_repr::execution_schema::ValueId(1), 0)]),
            [first, second],
        )
        .unwrap();
        Arc::new(image.instantiate().unwrap())
    }

    unsafe fn write_object(
        base: *mut u8,
        offset: usize,
        descriptor: &ObjectDescriptor,
        state: DescriptorState,
    ) -> *mut u8 {
        let object = base.add(offset);
        descriptor.initialize_header(object);
        std::ptr::write(
            object.cast::<usize>(),
            descriptor.initial_header_word() | state as usize,
        );
        object
    }

    #[test]
    fn w5_external_graph_shared_payload_and_remembered_slot_relocate_once() {
        use std::cell::UnsafeCell;
        struct Payload(UnsafeCell<*mut u8>);
        // SAFETY: the stack owner remains live through the copy; only the
        // collector accesses the cell after initial construction.
        unsafe impl ExternalPayloadOwner for Payload {
            fn slots(
                &self,
                published: *mut u8,
                kind: ExternalStorageKind,
            ) -> Result<ExternalPointerSlots, ExternalStorageValidationError> {
                if published != self.0.get().cast() {
                    return Err(ExternalStorageValidationError::Untracked(published));
                }
                if kind != ExternalStorageKind::BoxedArray {
                    return Err(ExternalStorageValidationError::KindMismatch {
                        expected: kind,
                        actual: ExternalStorageKind::BoxedArray,
                    });
                }
                Ok(unsafe { ExternalPointerSlots::from_validated(self.0.get(), 1) })
            }
        }
        let target = TargetDescriptor {
            architecture: Architecture::X86_64,
            endianness: Endianness::Little,
            pointer_width: 64,
            word_width: 64,
            abi: "system-v".into(),
            features: vec![],
        };
        let array =
            Arc::new(ObjectDescriptor::external(ExternalStorageKind::BoxedArray, &target).unwrap());
        let leaf = constructor_descriptor(1, &[]);
        assert_eq!(array.allocation_extent(), 16);
        let mut from = [0_u64; 6];
        let mut to = [0_u64; 6];
        let mut space = DescriptorSpace::new([array.clone(), leaf.clone()]).unwrap();
        unsafe {
            let base = from.as_mut_ptr().cast::<u8>();
            let first = write_object(base, 0, &array, DescriptorState::Live);
            let second = write_object(base, 16, &array, DescriptorState::Live);
            let child = write_object(base, 32, &leaf, DescriptorState::Live);
            let payload = Payload(UnsafeCell::new(
                (child as usize | usize::from(leaf.tag())) as *mut u8,
            ));
            let published = payload.0.get().cast::<u8>();
            first.add(8).cast::<*mut u8>().write(published);
            second.add(8).cast::<*mut u8>().write(published);
            let mut roots = [first, second];
            let slots = [
                roots.as_mut_ptr(),
                roots.as_mut_ptr().add(1),
                payload.0.get(),
            ];
            let copied = cheney_copy_descriptors_with_external(
                &slots,
                base,
                48,
                std::slice::from_raw_parts_mut(to.as_mut_ptr().cast(), 48),
                &mut space,
                None,
                &payload,
            )
            .unwrap();
            assert_eq!(copied.bytes_copied, 48);
            for root in roots {
                assert_eq!(root.add(8).cast::<*mut u8>().read(), published);
            }
            let moved = *payload.0.get() as usize;
            assert_eq!(tag_of(moved), leaf.tag());
            assert!((to.as_ptr() as usize..to.as_ptr() as usize + 48).contains(&untag(moved)));
            assert_eq!(leaf.state(child, 16).unwrap(), DescriptorState::Forwarded);
        }
    }

    #[test]
    fn descriptor_cheney_preserves_sharing_and_cycles() {
        let layout = descriptor(
            ObjectKind::Constructor,
            &[RuntimeRep::LiftedRef, RuntimeRep::LiftedRef],
        );
        let extent = layout.allocation_extent() as usize;
        let mut from = [0_u64; 16];
        let mut to = [0_u64; 16];
        let mut space = DescriptorSpace::new([Arc::clone(&layout)]).unwrap();
        unsafe {
            let first = write_object(from.as_mut_ptr().cast(), 0, &layout, DescriptorState::Live);
            let second = write_object(
                from.as_mut_ptr().cast(),
                extent,
                &layout,
                DescriptorState::Live,
            );
            std::ptr::write(first.add(8).cast::<*mut u8>(), second);
            std::ptr::write(first.add(16).cast::<*mut u8>(), second);
            std::ptr::write(second.add(8).cast::<*mut u8>(), first);
            std::ptr::write(second.add(16).cast::<*mut u8>(), std::ptr::null_mut());
            let mut root_a = first;
            let mut root_b = first;
            let roots = [&mut root_a as *mut *mut u8, &mut root_b];
            let copied = cheney_copy_descriptors(
                &roots,
                from.as_ptr().cast(),
                extent * 2,
                std::slice::from_raw_parts_mut(to.as_mut_ptr().cast(), extent * 2),
                &mut space,
            )
            .unwrap();
            assert_eq!(copied.bytes_copied, extent * 2);
            assert_eq!(root_a, root_b);
            assert_eq!(root_a, to.as_mut_ptr().cast());
            let child = std::ptr::read(root_a.add(8).cast::<*mut u8>());
            assert_eq!(child, std::ptr::read(root_a.add(16).cast::<*mut u8>()));
            assert_eq!(std::ptr::read(child.add(8).cast::<*mut u8>()), root_a);
        }
    }

    #[test]
    fn descriptor_cheney_accepts_duplicate_root_slot_address() {
        let layout = descriptor(ObjectKind::Constructor, &[]);
        let extent = layout.allocation_extent() as usize;
        let mut from = [0_u64; 4];
        let mut to = [0_u64; 4];
        let mut space = DescriptorSpace::new([Arc::clone(&layout)]).unwrap();
        unsafe {
            let object = write_object(from.as_mut_ptr().cast(), 0, &layout, DescriptorState::Live);
            let mut root = object;
            let slot = &mut root as *mut *mut u8;
            let copied = cheney_copy_descriptors(
                &[slot, slot],
                from.as_ptr().cast(),
                extent,
                std::slice::from_raw_parts_mut(to.as_mut_ptr().cast(), extent),
                &mut space,
            )
            .unwrap();
            assert_eq!(copied.bytes_copied, extent);
            assert_eq!(root, to.as_mut_ptr().cast());
        }
    }

    #[test]
    fn descriptor_cheney_preserves_managed_evidence_on_roots_and_fields() {
        let owner = constructor_descriptor(3, &[RuntimeRep::LiftedRef]);
        let child = constructor_descriptor(1, &[]);
        let owner_extent = owner.allocation_extent() as usize;
        let child_extent = child.allocation_extent() as usize;
        let mut from = [0_u64; 8];
        let mut to = [0_u64; 8];
        let mut space = DescriptorSpace::new([Arc::clone(&owner), Arc::clone(&child)]).unwrap();
        unsafe {
            let owner_ptr =
                write_object(from.as_mut_ptr().cast(), 0, &owner, DescriptorState::Live);
            let child_ptr = write_object(
                from.as_mut_ptr().cast(),
                owner_extent,
                &child,
                DescriptorState::Live,
            );
            std::ptr::write(
                owner_ptr
                    .add(owner.trace_offsets()[0] as usize)
                    .cast::<usize>(),
                child_ptr as usize | 1,
            );
            let mut root = (owner_ptr as usize | 3) as *mut u8;
            cheney_copy_descriptors(
                &[&mut root],
                from.as_ptr().cast(),
                owner_extent + child_extent,
                std::slice::from_raw_parts_mut(to.as_mut_ptr().cast(), to.len() * 8),
                &mut space,
            )
            .unwrap();
            assert_eq!(root as usize & 7, 3);
            assert_eq!(untag(root as usize), to.as_mut_ptr() as usize);
            let moved_child = std::ptr::read(
                (untag(root as usize) as *mut u8)
                    .add(owner.trace_offsets()[0] as usize)
                    .cast::<usize>(),
            );
            assert_eq!(moved_child & 7, 1);
            assert_eq!(untag(moved_child), to.as_mut_ptr() as usize + owner_extent);
        }
    }

    #[test]
    fn descriptor_cheney_rejects_updated_cycles_before_mutation() {
        let thunk = descriptor(ObjectKind::Thunk, &[RuntimeRep::LiftedRef]);
        let extent = thunk.allocation_extent() as usize;
        let mut from = [0_u64; 8];
        let mut to = [0_u64; 8];
        let mut space = DescriptorSpace::new([Arc::clone(&thunk)]).unwrap();
        unsafe {
            let first = write_object(
                from.as_mut_ptr().cast(),
                0,
                &thunk,
                DescriptorState::Updated,
            );
            let second = write_object(
                from.as_mut_ptr().cast(),
                extent,
                &thunk,
                DescriptorState::Updated,
            );
            std::ptr::write(first.add(8).cast::<*mut u8>(), second);
            std::ptr::write(second.add(8).cast::<*mut u8>(), first);
            let mut root = first;
            let error = cheney_copy_descriptors(
                &[&mut root],
                from.as_ptr().cast(),
                extent * 2,
                std::slice::from_raw_parts_mut(to.as_mut_ptr().cast(), extent * 2),
                &mut space,
            )
            .err()
            .unwrap();
            assert!(matches!(error, DescriptorTraceError::UpdatedCycle { .. }));
            assert_eq!(root, first);
            assert_eq!(to, [0_u64; 8]);
            assert_eq!(
                thunk.state(first, extent).unwrap(),
                DescriptorState::Updated
            );
            assert_eq!(
                thunk.state(second, extent).unwrap(),
                DescriptorState::Updated
            );
        }
    }

    #[test]
    fn descriptor_cheney_short_circuits_updated_thunks_with_different_extents() {
        let narrow_thunk = descriptor(ObjectKind::Thunk, &[]);
        let wide_thunk = descriptor(
            ObjectKind::Thunk,
            &[RuntimeRep::Address, RuntimeRep::Address],
        );
        let result = constructor_descriptor(1, &[]);
        let narrow_extent = narrow_thunk.allocation_extent() as usize;
        let wide_extent = wide_thunk.allocation_extent() as usize;
        let result_extent = result.allocation_extent() as usize;
        let mut from = [0_u64; 16];
        let mut to = [0_u64; 16];
        let mut space = DescriptorSpace::new([
            Arc::clone(&narrow_thunk),
            Arc::clone(&wide_thunk),
            Arc::clone(&result),
        ])
        .unwrap();
        unsafe {
            let first = write_object(
                from.as_mut_ptr().cast(),
                0,
                &narrow_thunk,
                DescriptorState::Updated,
            );
            let second = write_object(
                from.as_mut_ptr().cast(),
                narrow_extent,
                &wide_thunk,
                DescriptorState::Updated,
            );
            let final_value = write_object(
                from.as_mut_ptr().cast(),
                narrow_extent + wide_extent,
                &result,
                DescriptorState::Live,
            );
            std::ptr::write(first.add(8).cast::<*mut u8>(), second);
            std::ptr::write(second.add(8).cast::<usize>(), final_value as usize | 1);
            let mut root = first;
            let copied = cheney_copy_descriptors(
                &[&mut root],
                from.as_ptr().cast(),
                narrow_extent + wide_extent + result_extent,
                std::slice::from_raw_parts_mut(to.as_mut_ptr().cast(), to.len() * 8),
                &mut space,
            )
            .unwrap();
            assert_eq!(copied.bytes_copied, result_extent);
            assert_eq!(root as usize & 7, 1);
            assert_eq!(untag(root as usize), to.as_mut_ptr() as usize);
            assert_eq!(
                narrow_thunk.state(first, narrow_extent).unwrap(),
                DescriptorState::Forwarded
            );
            assert_eq!(
                wide_thunk.state(second, wide_extent).unwrap(),
                DescriptorState::Forwarded
            );
        }
    }

    #[test]
    fn descriptor_cheney_rejects_updated_targeting_forwarded_thunk() {
        let thunk = descriptor(ObjectKind::Thunk, &[]);
        let updated = descriptor(ObjectKind::Thunk, &[RuntimeRep::LiftedRef]);
        let thunk_extent = thunk.allocation_extent() as usize;
        let updated_extent = updated.allocation_extent() as usize;
        let mut from = [0_u64; 8];
        let mut to = [0_u64; 8];
        let mut space = DescriptorSpace::new([Arc::clone(&thunk), Arc::clone(&updated)]).unwrap();
        unsafe {
            let live_thunk =
                write_object(from.as_mut_ptr().cast(), 0, &thunk, DescriptorState::Live);
            let updated_thunk = write_object(
                from.as_mut_ptr().cast(),
                thunk_extent,
                &updated,
                DescriptorState::Updated,
            );
            std::ptr::write(updated_thunk.add(8).cast::<*mut u8>(), live_thunk);
            let mut live_root = live_thunk;
            let mut updated_root = updated_thunk;
            let error = cheney_copy_descriptors(
                &[
                    &mut live_root as *mut *mut u8,
                    &mut updated_root as *mut *mut u8,
                ],
                from.as_ptr().cast(),
                thunk_extent + updated_extent,
                std::slice::from_raw_parts_mut(to.as_mut_ptr().cast(), to.len() * 8),
                &mut space,
            )
            .err()
            .unwrap();
            assert_eq!(error, DescriptorTraceError::InvalidUpdatedTarget);
        }
    }

    #[test]
    fn descriptor_cheney_rejects_tagged_indirectee_after_updated_target_forwards() {
        let thunk = descriptor(ObjectKind::Thunk, &[]);
        let result = constructor_descriptor(3, &[]);
        let thunk_extent = thunk.allocation_extent() as usize;
        let result_extent = result.allocation_extent() as usize;
        let mut from = [0_u64; 8];
        let mut to = [0_u64; 8];
        let mut space = DescriptorSpace::new([Arc::clone(&thunk), Arc::clone(&result)]).unwrap();
        unsafe {
            let updated_a = write_object(
                from.as_mut_ptr().cast(),
                0,
                &thunk,
                DescriptorState::Updated,
            );
            let updated_b = write_object(
                from.as_mut_ptr().cast(),
                thunk_extent,
                &thunk,
                DescriptorState::Updated,
            );
            let final_value = write_object(
                from.as_mut_ptr().cast(),
                thunk_extent * 2,
                &result,
                DescriptorState::Live,
            );
            std::ptr::write(updated_b.add(8).cast::<usize>(), final_value as usize | 3);
            std::ptr::write(updated_a.add(8).cast::<usize>(), updated_b as usize | 3);
            let mut roots = [updated_b, updated_a];
            let error = cheney_copy_descriptors(
                &[roots.as_mut_ptr(), roots.as_mut_ptr().add(1)],
                from.as_ptr().cast(),
                thunk_extent * 2 + result_extent,
                std::slice::from_raw_parts_mut(to.as_mut_ptr().cast(), to.len() * 8),
                &mut space,
            )
            .err()
            .unwrap();
            assert_eq!(
                error,
                DescriptorTraceError::InvalidManagedTag {
                    address: updated_b as usize,
                    tag: 3,
                }
            );
            assert_eq!(roots[0] as usize & 7, 3);
            assert_eq!(untag(roots[0] as usize), to.as_mut_ptr() as usize);
            assert_eq!(
                thunk.state(updated_b, thunk_extent).unwrap(),
                DescriptorState::Forwarded
            );
            assert_eq!(
                thunk.state(updated_a, thunk_extent).unwrap(),
                DescriptorState::Updated
            );
        }
    }

    #[test]
    fn descriptor_cheney_resolves_long_updated_chain_with_reusable_path() {
        let thunk = descriptor(ObjectKind::Thunk, &[RuntimeRep::LiftedRef]);
        let result = constructor_descriptor(1, &[]);
        let thunk_extent = thunk.allocation_extent() as usize;
        let result_extent = result.allocation_extent() as usize;
        let chain_len = 24;
        let source_bytes = thunk_extent * chain_len + result_extent;
        let mut from = vec![0_u64; source_bytes / 8];
        let mut to = vec![0_u64; source_bytes / 8];
        let mut space = DescriptorSpace::new([Arc::clone(&thunk), Arc::clone(&result)]).unwrap();
        unsafe {
            let mut nodes = Vec::with_capacity(chain_len);
            for index in 0..chain_len {
                nodes.push(write_object(
                    from.as_mut_ptr().cast(),
                    thunk_extent * index,
                    &thunk,
                    DescriptorState::Updated,
                ));
            }
            let final_value = write_object(
                from.as_mut_ptr().cast(),
                thunk_extent * chain_len,
                &result,
                DescriptorState::Live,
            );
            for pair in nodes.windows(2) {
                std::ptr::write(pair[0].add(8).cast::<*mut u8>(), pair[1]);
            }
            std::ptr::write(
                nodes[chain_len - 1].add(8).cast::<usize>(),
                final_value as usize | 1,
            );
            let mut root = nodes[0];
            let copied = cheney_copy_descriptors(
                &[&mut root],
                from.as_ptr().cast(),
                source_bytes,
                std::slice::from_raw_parts_mut(to.as_mut_ptr().cast(), source_bytes),
                &mut space,
            )
            .unwrap();
            assert_eq!(copied.bytes_copied, result_extent);
            assert_eq!(root as usize & 7, 1);
            assert_eq!(untag(root as usize), to.as_mut_ptr() as usize);
            for node in nodes {
                assert_eq!(
                    thunk.state(node, thunk_extent).unwrap(),
                    DescriptorState::Forwarded
                );
            }
        }
    }

    #[test]
    fn descriptor_cheney_grows_updated_path_before_larger_collection() {
        let thunk = descriptor(ObjectKind::Thunk, &[RuntimeRep::LiftedRef]);
        let result = constructor_descriptor(1, &[]);
        let thunk_extent = thunk.allocation_extent() as usize;
        let result_extent = result.allocation_extent() as usize;
        let mut space = DescriptorSpace::new([Arc::clone(&thunk), Arc::clone(&result)]).unwrap();

        unsafe fn collect_updated_chain(
            space: &mut DescriptorSpace,
            thunk: &ObjectDescriptor,
            result: &ObjectDescriptor,
            chain_len: usize,
        ) {
            let thunk_extent = thunk.allocation_extent() as usize;
            let result_extent = result.allocation_extent() as usize;
            let source_bytes = thunk_extent * chain_len + result_extent;
            let mut from = vec![0_u64; source_bytes / 8];
            let mut to = vec![0_u64; source_bytes / 8];
            let mut nodes = Vec::with_capacity(chain_len);
            for index in 0..chain_len {
                nodes.push(write_object(
                    from.as_mut_ptr().cast(),
                    thunk_extent * index,
                    thunk,
                    DescriptorState::Updated,
                ));
            }
            let final_value = write_object(
                from.as_mut_ptr().cast(),
                thunk_extent * chain_len,
                result,
                DescriptorState::Live,
            );
            for pair in nodes.windows(2) {
                std::ptr::write(pair[0].add(8).cast::<*mut u8>(), pair[1]);
            }
            std::ptr::write(
                nodes[chain_len - 1].add(8).cast::<usize>(),
                final_value as usize | 1,
            );
            let mut root = nodes[0];
            let copied = cheney_copy_descriptors(
                &[&mut root],
                from.as_ptr().cast(),
                source_bytes,
                std::slice::from_raw_parts_mut(to.as_mut_ptr().cast(), source_bytes),
                space,
            )
            .unwrap();
            assert_eq!(copied.bytes_copied, result_extent);
            assert_eq!(root as usize & 7, 1);
        }

        unsafe {
            collect_updated_chain(&mut space, &thunk, &result, 1);
        }
        let larger_chain_len = 24;
        let larger_source_bytes = thunk_extent * larger_chain_len + result_extent;
        space.prepare_starts(larger_source_bytes).unwrap();
        assert!(
            space.updated_path.capacity() >= larger_source_bytes.div_ceil(16),
            "updated path scratch must be ready before the larger collection mutates heap objects"
        );
        unsafe {
            collect_updated_chain(&mut space, &thunk, &result, larger_chain_len);
        }
    }

    #[test]
    fn descriptor_cheney_preserves_nursery_edges_to_static_objects() {
        let static_region = static_cycle();
        let nursery = descriptor(ObjectKind::Constructor, &[RuntimeRep::LiftedRef]);
        let extent = nursery.allocation_extent() as usize;
        let mut from = [0_u64; 4];
        let mut to = [0_u64; 4];
        let mut space = DescriptorSpace::new([Arc::clone(&nursery)])
            .unwrap()
            .with_static_region(Arc::clone(&static_region));
        unsafe {
            let object = write_object(from.as_mut_ptr().cast(), 0, &nursery, DescriptorState::Live);
            let static_root = static_region
                .entry(tidepool_repr::execution_schema::ValueId(1))
                .unwrap();
            std::ptr::write(object.add(8).cast::<usize>(), static_root);
            let mut root = object;
            let copied = cheney_copy_descriptors(
                &[&mut root],
                from.as_ptr().cast(),
                extent,
                std::slice::from_raw_parts_mut(to.as_mut_ptr().cast(), to.len() * 8),
                &mut space,
            )
            .unwrap();
            assert_eq!(copied.bytes_copied, extent);
            assert_eq!(untag(root as usize), to.as_mut_ptr() as usize);
            assert_eq!(std::ptr::read(root.add(8).cast::<usize>()), static_root);
        }
    }

    #[test]
    fn descriptor_cheney_accepts_cyclic_static_root_without_copying() {
        let static_region = static_cycle();
        let mut from = [0_u64; 2];
        let mut to = [0_u64; 2];
        let mut space = DescriptorSpace::new([])
            .unwrap()
            .with_static_region(Arc::clone(&static_region));
        let mut root = static_region
            .entry(tidepool_repr::execution_schema::ValueId(1))
            .unwrap() as *mut u8;
        unsafe {
            let copied = cheney_copy_descriptors(
                &[&mut root],
                from.as_ptr().cast(),
                0,
                std::slice::from_raw_parts_mut(to.as_mut_ptr().cast(), to.len() * 8),
                &mut space,
            )
            .unwrap();
            assert_eq!(copied.bytes_copied, 0);
            assert_eq!(
                root as usize,
                static_region
                    .entry(tidepool_repr::execution_schema::ValueId(1))
                    .unwrap()
            );
        }
    }

    #[test]
    fn descriptor_cheney_rejects_static_interior_and_tagged_edges() {
        let static_region = static_cycle();
        let entry = static_region
            .entry(tidepool_repr::execution_schema::ValueId(1))
            .unwrap();
        let mut from = [0_u64; 2];
        let mut to = [0_u64; 2];
        for (value, expected) in [
            (
                untag(entry) + 8,
                DescriptorTraceError::InvalidManagedPointer {
                    address: untag(entry) + 8,
                },
            ),
            (
                entry | 2,
                DescriptorTraceError::InvalidManagedTag {
                    address: untag(entry),
                    tag: 3,
                },
            ),
        ] {
            let mut space = DescriptorSpace::new([])
                .unwrap()
                .with_static_region(Arc::clone(&static_region));
            let mut root = value as *mut u8;
            unsafe {
                let error = cheney_copy_descriptors(
                    &[&mut root],
                    from.as_ptr().cast(),
                    0,
                    std::slice::from_raw_parts_mut(to.as_mut_ptr().cast(), to.len() * 8),
                    &mut space,
                )
                .err()
                .unwrap();
                assert_eq!(error, expected);
                assert_eq!(root as usize, value);
            }
        }
    }

    #[test]
    fn descriptor_cheney_rejects_root_slots_inside_static_region_before_mutation() {
        let static_region = static_cycle();
        let entry = static_region
            .entry(tidepool_repr::execution_schema::ValueId(1))
            .unwrap();
        let slot = (untag(entry) + 8) as *mut *mut u8;
        let before = unsafe { std::ptr::read(slot) };
        let mut from = [0_u64; 2];
        let mut to = [0_u64; 2];
        let mut space = DescriptorSpace::new([])
            .unwrap()
            .with_static_region(Arc::clone(&static_region));
        unsafe {
            let error = cheney_copy_descriptors(
                &[slot],
                from.as_ptr().cast(),
                0,
                std::slice::from_raw_parts_mut(to.as_mut_ptr().cast(), to.len() * 8),
                &mut space,
            )
            .err()
            .unwrap();
            assert_eq!(error, DescriptorTraceError::InvalidRange);
            assert_eq!(std::ptr::read(slot), before);
        }
    }

    #[test]
    fn descriptor_cheney_forwards_updated_chains_to_static_objects() {
        let static_region = static_cycle();
        let thunk = descriptor(ObjectKind::Thunk, &[RuntimeRep::LiftedRef]);
        let extent = thunk.allocation_extent() as usize;
        let source_bytes = extent * 2;
        let mut from = vec![0_u64; source_bytes / 8];
        let mut to = vec![0_u64; source_bytes / 8];
        let mut space = DescriptorSpace::new([Arc::clone(&thunk)])
            .unwrap()
            .with_static_region(Arc::clone(&static_region));
        unsafe {
            let first = write_object(
                from.as_mut_ptr().cast(),
                0,
                &thunk,
                DescriptorState::Updated,
            );
            let second = write_object(
                from.as_mut_ptr().cast(),
                extent,
                &thunk,
                DescriptorState::Updated,
            );
            let static_root = static_region
                .entry(tidepool_repr::execution_schema::ValueId(1))
                .unwrap();
            std::ptr::write(first.add(8).cast::<usize>(), second as usize);
            std::ptr::write(second.add(8).cast::<usize>(), static_root);
            let mut roots = [first, second];
            let copied = cheney_copy_descriptors(
                &[roots.as_mut_ptr(), roots.as_mut_ptr().add(1)],
                from.as_ptr().cast(),
                source_bytes,
                std::slice::from_raw_parts_mut(to.as_mut_ptr().cast(), to.len() * 8),
                &mut space,
            )
            .unwrap();
            assert_eq!(copied.bytes_copied, 0);
            assert_eq!(roots[0] as usize, static_root);
            assert_eq!(roots[1] as usize, static_root);
            assert_eq!(std::ptr::read(first.add(8).cast::<usize>()), static_root);
            assert_eq!(std::ptr::read(second.add(8).cast::<usize>()), static_root);
            assert_eq!(
                thunk.state(first, extent).unwrap(),
                DescriptorState::Forwarded
            );
            assert_eq!(
                thunk.state(second, extent).unwrap(),
                DescriptorState::Forwarded
            );
        }
    }

    #[test]
    fn descriptor_cheney_forwards_updated_thunk_to_admitted_old_value() {
        let thunk = descriptor(ObjectKind::Thunk, &[]);
        let result = constructor_descriptor(1, &[]);
        let thunk_extent = thunk.allocation_extent() as usize;
        let result_extent = result.allocation_extent() as usize;
        let mut from = vec![0_u64; thunk_extent / 8];
        let mut to = vec![0_u64; thunk_extent / 8];
        let mut old = DescriptorArena::reserve(result_extent, [Arc::clone(&result)]).unwrap();
        let old_value = unsafe {
            let object = old.destination().as_mut_ptr();
            result.initialize_header(object);
            std::ptr::write(
                object.cast::<usize>(),
                result.initial_header_word() | DescriptorState::Live as usize,
            );
            object
        };
        old.seal(result_extent).unwrap();
        let mut space = DescriptorSpace::new([Arc::clone(&thunk), Arc::clone(&result)]).unwrap();
        unsafe {
            let updated = write_object(
                from.as_mut_ptr().cast(),
                0,
                &thunk,
                DescriptorState::Updated,
            );
            let stored = old_value as usize | usize::from(result.tag());
            std::ptr::write(updated.add(8).cast::<usize>(), stored);
            let mut root = updated;
            let copied = cheney_copy_descriptors_with_admission(
                &[&mut root],
                from.as_ptr().cast(),
                thunk_extent,
                std::slice::from_raw_parts_mut(to.as_mut_ptr().cast(), to.len() * 8),
                &mut space,
                &old,
            )
            .unwrap();
            assert_eq!(copied.bytes_copied, 0);
            assert_eq!(root as usize, stored);
            assert_eq!(std::ptr::read(updated.add(8).cast::<usize>()), stored);
            assert_eq!(
                thunk.state(updated, thunk_extent).unwrap(),
                DescriptorState::Forwarded
            );
        }
    }

    #[test]
    fn descriptor_cheney_rejects_root_slot_inside_heap_before_mutation() {
        let layout = descriptor(ObjectKind::Constructor, &[RuntimeRep::LiftedRef]);
        let extent = layout.allocation_extent() as usize;
        let mut from = [0_u64; 4];
        let mut to = [0_u64; 4];
        let mut space = DescriptorSpace::new([Arc::clone(&layout)]).unwrap();
        unsafe {
            let object = write_object(from.as_mut_ptr().cast(), 0, &layout, DescriptorState::Live);
            std::ptr::write(object.add(8).cast::<*mut u8>(), object);
            let error = cheney_copy_descriptors(
                &[object.add(8).cast::<*mut u8>()],
                from.as_ptr().cast(),
                extent,
                std::slice::from_raw_parts_mut(to.as_mut_ptr().cast(), extent),
                &mut space,
            )
            .err()
            .unwrap();
            assert_eq!(error, DescriptorTraceError::InvalidRange);
            assert_eq!(layout.state(object, extent).unwrap(), DescriptorState::Live);
            assert_eq!(std::ptr::read(object.add(8).cast::<*mut u8>()), object);
        }
    }

    #[test]
    fn descriptor_cheney_rejects_forged_header_at_interior_pointer() {
        let layout = descriptor(ObjectKind::Constructor, &[RuntimeRep::Address]);
        let extent = layout.allocation_extent() as usize;
        let mut from = [0_u64; 8];
        let mut to = [0_u64; 8];
        let mut space = DescriptorSpace::new([Arc::clone(&layout)]).unwrap();
        unsafe {
            let object = write_object(from.as_mut_ptr().cast(), 0, &layout, DescriptorState::Live);
            std::ptr::write(object.add(8).cast::<usize>(), layout.initial_header_word());
            let interior = object.add(8);
            let mut root = interior;
            let error = cheney_copy_descriptors(
                &[&mut root],
                from.as_ptr().cast(),
                extent,
                std::slice::from_raw_parts_mut(to.as_mut_ptr().cast(), extent),
                &mut space,
            )
            .err()
            .unwrap();
            assert!(matches!(
                error,
                DescriptorTraceError::InvalidManagedPointer { address } if address == interior as usize
            ));
            assert_eq!(root, interior);
            assert_eq!(layout.state(object, extent).unwrap(), DescriptorState::Live);
        }
    }

    #[test]
    fn descriptor_cheney_late_bad_edge_reports_after_relocation() {
        let layout = descriptor(ObjectKind::Constructor, &[RuntimeRep::LiftedRef]);
        let extent = layout.allocation_extent() as usize;
        let mut from = [0_u64; 8];
        let mut to = [0_u64; 8];
        let mut space = DescriptorSpace::new([Arc::clone(&layout)]).unwrap();
        unsafe {
            let first = write_object(from.as_mut_ptr().cast(), 0, &layout, DescriptorState::Live);
            let second = write_object(
                from.as_mut_ptr().cast(),
                extent,
                &layout,
                DescriptorState::Live,
            );
            std::ptr::write(first.add(8).cast::<*mut u8>(), second);
            std::ptr::write(second.add(8).cast::<*mut u8>(), first.add(8));
            let mut root = first;
            let error = cheney_copy_descriptors(
                &[&mut root],
                from.as_ptr().cast(),
                extent * 2,
                std::slice::from_raw_parts_mut(to.as_mut_ptr().cast(), extent * 2),
                &mut space,
            )
            .err()
            .unwrap();
            assert!(matches!(
                error,
                DescriptorTraceError::InvalidManagedPointer { address } if address == first.add(8) as usize
            ));
            assert_eq!(root, to.as_mut_ptr().cast());
            assert_eq!(
                layout.state(first, extent).unwrap(),
                DescriptorState::Forwarded
            );
            assert_eq!(
                layout.state(second, extent).unwrap(),
                DescriptorState::Forwarded
            );
        }
    }

    #[test]
    fn descriptor_cheney_retains_states_and_traces_only_managed_slots() {
        let live_layout = descriptor(
            ObjectKind::Constructor,
            &[RuntimeRep::Address, RuntimeRep::LiftedRef],
        );
        let thunk_layout = descriptor(
            ObjectKind::Thunk,
            &[RuntimeRep::Address, RuntimeRep::LiftedRef],
        );
        let extent = live_layout.allocation_extent() as usize;
        let mut from = [0_u64; 16];
        let mut to = [0_u64; 16];
        let mut space =
            DescriptorSpace::new([Arc::clone(&live_layout), Arc::clone(&thunk_layout)]).unwrap();
        unsafe {
            let live = write_object(
                from.as_mut_ptr().cast(),
                0,
                &live_layout,
                DescriptorState::Live,
            );
            let evaluating = write_object(
                from.as_mut_ptr().cast(),
                extent,
                &thunk_layout,
                DescriptorState::Evaluating,
            );
            let updated = write_object(
                from.as_mut_ptr().cast(),
                extent * 2,
                &thunk_layout,
                DescriptorState::Updated,
            );
            std::ptr::write(live.add(8).cast::<usize>(), 0x1234_5678);
            std::ptr::write(live.add(16).cast::<*mut u8>(), evaluating);
            std::ptr::write(evaluating.add(8).cast::<usize>(), 0x1234_5678);
            std::ptr::write(evaluating.add(16).cast::<*mut u8>(), live);
            std::ptr::write(updated.add(8).cast::<*mut u8>(), live);
            std::ptr::write(updated.add(16).cast::<usize>(), usize::MAX);
            let mut live_root = live;
            let mut updated_root = updated;
            let roots = [&mut live_root as *mut *mut u8, &mut updated_root];
            let copied = cheney_copy_descriptors(
                &roots,
                from.as_ptr().cast(),
                extent * 3,
                std::slice::from_raw_parts_mut(to.as_mut_ptr().cast(), extent * 3),
                &mut space,
            )
            .unwrap();
            assert_eq!(copied.bytes_copied, extent * 2);
            assert_eq!(updated_root, live_root);
            let copied_eval = std::ptr::read(live_root.add(16).cast::<*mut u8>());
            assert_eq!(
                live_layout.state(live_root, extent).unwrap(),
                DescriptorState::Live
            );
            assert_eq!(
                thunk_layout.state(copied_eval, extent).unwrap(),
                DescriptorState::Evaluating
            );
            assert_eq!(
                std::ptr::read(copied_eval.add(16).cast::<*mut u8>()),
                live_root
            );
            assert_eq!(
                std::ptr::read(copied_eval.add(8).cast::<usize>()),
                0x1234_5678
            );
            assert_eq!(
                live_layout.state(live, extent).unwrap(),
                DescriptorState::Forwarded
            );
            assert_eq!(
                thunk_layout.state(evaluating, extent).unwrap(),
                DescriptorState::Forwarded
            );
            assert_eq!(
                thunk_layout.state(updated, extent).unwrap(),
                DescriptorState::Forwarded
            );
        }
    }
}
