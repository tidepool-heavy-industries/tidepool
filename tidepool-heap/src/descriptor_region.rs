//! Stable mutable descriptor arenas owned by the runtime's OldSpace.
//!
//! Region metadata describes allocation starts, not individual live-object
//! registrations. Allocate before promotion writes; keep even a failed partial
//! destination owned through native unwind. Only seal a complete copied prefix.

use crate::execution_descriptor::{DescriptorState, DescriptorTraceError, ObjectDescriptor};
use crate::managed_reference::{tag_of, tag_valid, untag};
use std::{cell::UnsafeCell, collections::HashMap, ops::Range, sync::Arc};

/// Collector-borrowed admission from the existing OldSpace owner.
///
/// # Safety
/// An admitted address is an exact initialized start with a valid header/tag,
/// backed by stable writable storage and pinned descriptors for the complete
/// collection/native unwind. No concurrent mutation or owner disposal occurs.
/// None means outside all owned allocations, not an invalid interior pointer.
pub unsafe trait DescriptorOldSpace {
    fn admit(&self, encoded: usize) -> Result<Option<usize>, DescriptorTraceError>;
}

/// Collector-borrowed SOURCE membership: the allocations one copy is
/// evacuating objects out of.
///
/// [`DescriptorOldSpace::admit`] proves a *stable* target and therefore
/// rejects a `Forwarded` header; a source space answers the opposite
/// question ("is this an object I am moving?"), where a forwarded header is
/// the expected in-progress state the copier itself wrote. Promotion's source
/// is one contiguous nursery range; compaction's is a set of retiring arenas.
/// Both drive the same Cheney loop.
///
/// # Safety
/// Every reported start is an exact initialized allocation start readable for
/// the returned extent, backed by stable writable storage with pinned
/// descriptors for the complete collection and native unwind. The reported
/// membership, storage bounds and byte total do not change during a copy.
pub unsafe trait DescriptorSourceSpace {
    /// `Ok(Some(available))` when `address` is an exact allocation start with
    /// `available` readable bytes from it, `Ok(None)` when it lies outside
    /// every source allocation, and `Err` when it points inside one without
    /// being a start.
    fn locate_start(&self, address: usize) -> Result<Option<usize>, DescriptorTraceError>;

    /// Whether the pointer-sized slot at `address` overlaps source storage.
    /// Root slots may not live in storage this copy is about to retire.
    fn covers_slot(&self, address: usize) -> bool;

    /// Whether `[start, end)` overlaps source storage, for the copier's
    /// source/destination disjointness proof.
    fn overlaps_range(&self, start: usize, end: usize) -> bool;

    /// Sealed bytes this source holds; the destination's size requirement.
    fn source_bytes(&self) -> usize;
}

pub struct DescriptorArena {
    words: Box<[UnsafeCell<u64>]>,
    used: usize,
    starts: Vec<u64>,
    descriptors: HashMap<usize, Arc<ObjectDescriptor>>,
}

impl DescriptorArena {
    /// Reserve allocation and exact-start scratch before any source forwarding.
    /// Installing this owner in OldSpace precedes handing its buffer to GC.
    pub fn reserve(
        bytes: usize,
        descriptors: impl IntoIterator<Item = Arc<ObjectDescriptor>>,
    ) -> Result<Self, DescriptorTraceError> {
        let count = bytes
            .checked_add(7)
            .ok_or(DescriptorTraceError::InvalidRange)?
            / 8;
        let mut words = Vec::new();
        words
            .try_reserve_exact(count)
            .map_err(|_| DescriptorTraceError::MetadataAllocation)?;
        words.resize_with(count, || UnsafeCell::new(0));
        let mut starts = Vec::new();
        let bits = count.div_ceil(64);
        starts
            .try_reserve_exact(bits)
            .map_err(|_| DescriptorTraceError::MetadataAllocation)?;
        starts.resize(bits, 0);
        let mut owners = HashMap::new();
        for descriptor in descriptors {
            owners
                .try_reserve(1)
                .map_err(|_| DescriptorTraceError::MetadataAllocation)?;
            owners.insert(descriptor.initial_header_word(), descriptor);
        }
        Ok(Self {
            words: words.into_boxed_slice(),
            used: 0,
            starts,
            descriptors: owners,
        })
    }

    /// Bytes of sealed objects this arena holds.
    pub fn bytes_used(&self) -> usize {
        self.used
    }

    pub fn allocation_range(&self) -> Range<usize> {
        let base = self.words.as_ptr() as usize;
        base..base + self.words.len() * 8
    }

    /// No managed address has been published yet. The collector fills this
    /// prefix while OldSpace keeps the allocation alive on both success/error.
    pub fn destination(&mut self) -> &mut [u8] {
        unsafe {
            std::slice::from_raw_parts_mut(self.words.as_mut_ptr().cast(), self.words.len() * 8)
        }
    }

    /// Validate headers and build exact starts without further allocation.
    /// Edges were checked by copying; this operation proves allocation shape.
    pub fn seal(&mut self, used: usize) -> Result<(), DescriptorTraceError> {
        if used > self.words.len() * 8 || !used.is_multiple_of(8) {
            return Err(DescriptorTraceError::InvalidRange);
        }
        self.starts.fill(0);
        let mut offset = 0;
        while offset < used {
            let object = unsafe { self.words.as_ptr().cast::<u8>().add(offset) };
            let header = unsafe { object.cast::<usize>().read() };
            let descriptor = self.descriptors.get(&(header & !7)).ok_or(
                DescriptorTraceError::UnknownDescriptor {
                    address: header & !7,
                },
            )?;
            let extent = descriptor.allocation_extent() as usize;
            let state = unsafe { descriptor.state(object, used - offset) }?;
            if state == DescriptorState::Forwarded {
                return Err(DescriptorTraceError::ForwardedObject);
            }
            if extent < 16 || !extent.is_multiple_of(8) || extent > used - offset {
                return Err(DescriptorTraceError::InvalidRange);
            }
            self.starts[offset / 8 / 64] |= 1 << (offset / 8 % 64);
            offset += extent;
        }
        self.used = used;
        Ok(())
    }

    /// Exact-start membership for an evacuating copy, with the readable
    /// extent bound. Deliberately header-free: an object this copy has
    /// already forwarded is still one of its own source objects, so unlike
    /// [`Self::admit`] a `Forwarded` state is not a failure here.
    pub fn locate_start(&self, address: usize) -> Result<Option<usize>, DescriptorTraceError> {
        let range = self.allocation_range();
        if !range.contains(&address) {
            return Ok(None);
        }
        let offset = address - range.start;
        if offset >= self.used
            || !offset.is_multiple_of(8)
            || self.starts[offset / 8 / 64] & (1 << (offset / 8 % 64)) == 0
        {
            return Err(DescriptorTraceError::InvalidManagedPointer { address });
        }
        Ok(Some(self.used - offset))
    }

    /// Visit every sealed object in allocation order with its pinned
    /// descriptor. [`Self::seal`] has already proven this walk; it allocates
    /// nothing, so a caller may run it after a copy, where nothing may fail.
    pub fn walk_sealed(
        &self,
        mut visit: impl FnMut(*mut u8, &ObjectDescriptor) -> Result<(), DescriptorTraceError>,
    ) -> Result<(), DescriptorTraceError> {
        let base = self.words.as_ptr().cast::<u8>().cast_mut();
        let mut offset = 0;
        while offset < self.used {
            // SAFETY: `offset` stays inside the sealed prefix, whose object
            // starts and extents `seal` validated against these descriptors.
            let object = unsafe { base.add(offset) };
            let header = unsafe { object.cast::<usize>().read() };
            let descriptor = self.descriptors.get(&(header & !7)).ok_or(
                DescriptorTraceError::UnknownDescriptor {
                    address: header & !7,
                },
            )?;
            let extent = descriptor.allocation_extent() as usize;
            if extent < 16 || !extent.is_multiple_of(8) || extent > self.used - offset {
                return Err(DescriptorTraceError::InvalidRange);
            }
            visit(object, descriptor)?;
            offset += extent;
        }
        Ok(())
    }

    /// Old objects may change Live/Evaluating/Updated state, but never extent
    /// or allocation start. Check the current header only after the bitmap.
    pub fn admit(&self, encoded: usize) -> Result<Option<usize>, DescriptorTraceError> {
        let range = self.allocation_range();
        let address = untag(encoded);
        if !range.contains(&address) {
            return Ok(None);
        }
        let offset = address - range.start;
        if offset >= self.used
            || !offset.is_multiple_of(8)
            || self.starts[offset / 8 / 64] & (1 << (offset / 8 % 64)) == 0
        {
            return Err(DescriptorTraceError::InvalidManagedPointer { address });
        }
        let header = unsafe { (address as *const usize).read() };
        let descriptor = self.descriptors.get(&(header & !7)).ok_or(
            DescriptorTraceError::UnknownDescriptor {
                address: header & !7,
            },
        )?;
        let state = unsafe { descriptor.state(address as *const u8, self.used - offset) }?;
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

// SAFETY: an arena's allocation and descriptor owners are stable for the
// duration of a collector borrow; admission never mutates the region.
unsafe impl DescriptorOldSpace for DescriptorArena {
    fn admit(&self, encoded: usize) -> Result<Option<usize>, DescriptorTraceError> {
        DescriptorArena::admit(self, encoded)
    }
}

// SAFETY: an arena's allocation is stable for the duration of a collector
// borrow, and its sealed starts/extents were proven by `seal`.
unsafe impl DescriptorSourceSpace for DescriptorArena {
    fn locate_start(&self, address: usize) -> Result<Option<usize>, DescriptorTraceError> {
        DescriptorArena::locate_start(self, address)
    }

    fn covers_slot(&self, address: usize) -> bool {
        self.overlaps_range(
            address,
            address.saturating_add(std::mem::size_of::<*mut u8>()),
        )
    }

    fn overlaps_range(&self, start: usize, end: usize) -> bool {
        let range = self.allocation_range();
        start < range.end && range.start < end
    }

    fn source_bytes(&self) -> usize {
        self.used
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution_descriptor::ObjectKind;
    use tidepool_repr::execution_schema::{testing, RuntimeRep, StorageLayout};

    #[test]
    fn w5_a5_region_rejects_descriptor_shaped_interior_words() {
        let descriptor = Arc::new(
            ObjectDescriptor::constructor(
                1,
                StorageLayout::for_reps(&testing::target(), &[RuntimeRep::Address]).unwrap(),
                None,
            )
            .unwrap(),
        );
        let mut arena = DescriptorArena::reserve(32, [Arc::clone(&descriptor)]).unwrap();
        let base = arena.destination().as_mut_ptr();
        unsafe {
            descriptor.initialize_header(base);
            // A scalar word can equal a pinned descriptor. Only the allocation
            // walk, not header-looking content, establishes an object start.
            base.add(8)
                .cast::<usize>()
                .write(descriptor.initial_header_word());
            descriptor.initialize_header(base.add(16));
        }
        arena.seal(32).unwrap();
        assert_eq!(
            arena.admit(base as usize | 1).unwrap(),
            Some(base as usize | 1)
        );
        assert!(matches!(
            arena.admit(base as usize + 8),
            Err(DescriptorTraceError::InvalidManagedPointer { .. })
        ));
        assert!(matches!(
            arena.admit(base as usize | 2),
            Err(DescriptorTraceError::InvalidManagedTag { .. })
        ));
        assert_eq!(descriptor.kind(), ObjectKind::Constructor);
    }
}
