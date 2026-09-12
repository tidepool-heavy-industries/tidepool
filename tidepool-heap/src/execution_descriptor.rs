use tidepool_repr::execution_schema::{LayoutError, Signature, StorageLayout};

use crate::layout::{self, HeapTag, HEADER_SIZE};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectKind {
    Function,
    Pap,
    Thunk,
    Constructor,
    Continuation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntryMetadata {
    signature: Signature,
    code_identity: u64,
}

impl EntryMetadata {
    pub fn new(signature: Signature, code_identity: u64) -> Self {
        Self {
            signature,
            code_identity,
        }
    }

    pub fn signature(&self) -> &Signature {
        &self.signature
    }

    pub fn code_identity(&self) -> u64 {
        self.code_identity
    }
}

/// Immutable physical description shared by allocation, tracing and emission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectDescriptor {
    kind: ObjectKind,
    payload: StorageLayout,
    entry: Option<EntryMetadata>,
    payload_base: u32,
    allocation_alignment: u32,
    allocation_extent: u32,
    trace_offsets: Vec<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum DescriptorTraceError {
    #[error("descriptor object extent {declared} exceeds readable bytes {available}")]
    Truncated { declared: u32, available: usize },
    #[error("descriptor trace slot at offset {offset} exceeds object extent {extent}")]
    InvalidOffset { offset: u32, extent: u32 },
}

impl ObjectDescriptor {
    pub fn new(
        kind: ObjectKind,
        payload: StorageLayout,
        entry: Option<EntryMetadata>,
    ) -> Result<Self, LayoutError> {
        let header_size = u32::try_from(HEADER_SIZE).map_err(|_| LayoutError::Overflow)?;
        let allocation_alignment = payload.alignment().max(header_size.next_power_of_two());
        let payload_base = descriptor_align_up(header_size, payload.alignment())?;
        let unrounded = payload_base
            .checked_add(payload.payload_size())
            .ok_or(LayoutError::Overflow)?
            .max(16);
        let allocation_extent = descriptor_align_up(unrounded, allocation_alignment)?;
        let trace_offsets = payload
            .managed_root_offsets()
            .iter()
            .map(|offset| {
                payload_base
                    .checked_add(*offset)
                    .ok_or(LayoutError::Overflow)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            kind,
            payload,
            entry,
            payload_base,
            allocation_alignment,
            allocation_extent,
            trace_offsets,
        })
    }

    pub fn kind(&self) -> ObjectKind {
        self.kind
    }

    pub fn payload(&self) -> &StorageLayout {
        &self.payload
    }

    pub fn entry(&self) -> Option<&EntryMetadata> {
        self.entry.as_ref()
    }

    pub fn payload_base(&self) -> u32 {
        self.payload_base
    }

    pub fn allocation_alignment(&self) -> u32 {
        self.allocation_alignment
    }

    pub fn allocation_extent(&self) -> u32 {
        self.allocation_extent
    }

    pub fn trace_offsets(&self) -> &[u32] {
        &self.trace_offsets
    }

    pub fn heap_tag(&self) -> HeapTag {
        match self.kind {
            ObjectKind::Function | ObjectKind::Pap | ObjectKind::Continuation => HeapTag::Closure,
            ObjectKind::Thunk => HeapTag::Thunk,
            ObjectKind::Constructor => HeapTag::Con,
        }
    }

    /// Initialize the existing collector-owned header from this descriptor.
    /// Payload initialization remains the caller's responsibility and must not
    /// begin unless allocation succeeded.
    ///
    /// # Safety
    ///
    /// `ptr` must name writable storage of at least `allocation_extent` bytes.
    pub unsafe fn initialize_header(&self, ptr: *mut u8) {
        layout::write_header(ptr, self.heap_tag().as_byte(), self.allocation_extent);
    }

    /// Visit exactly the managed-reference slots derived by `StorageLayout`.
    /// Address and numeric fields are never reclassified by inspecting their
    /// bits. The caller supplies this descriptor from the compiled entry's
    /// stable metadata; legacy tag/count scanning is not consulted.
    ///
    /// # Safety
    ///
    /// `ptr` must name an initialized object described by `self`, readable and
    /// writable for `available` bytes. Every visited reference slot must be
    /// initialized before this call.
    pub unsafe fn for_each_trace_slot(
        &self,
        ptr: *mut u8,
        available: usize,
        mut visit: impl FnMut(*mut *mut u8),
    ) -> Result<(), DescriptorTraceError> {
        if available < self.allocation_extent as usize {
            return Err(DescriptorTraceError::Truncated {
                declared: self.allocation_extent,
                available,
            });
        }
        for &offset in &self.trace_offsets {
            let slot_size = u32::try_from(std::mem::size_of::<*mut u8>()).map_err(|_| {
                DescriptorTraceError::InvalidOffset {
                    offset,
                    extent: self.allocation_extent,
                }
            })?;
            let end = offset
                .checked_add(slot_size)
                .ok_or(DescriptorTraceError::InvalidOffset {
                    offset,
                    extent: self.allocation_extent,
                })?;
            if end > self.allocation_extent {
                return Err(DescriptorTraceError::InvalidOffset {
                    offset,
                    extent: self.allocation_extent,
                });
            }
            visit(ptr.add(offset as usize) as *mut *mut u8);
        }
        Ok(())
    }
}

fn descriptor_align_up(value: u32, alignment: u32) -> Result<u32, LayoutError> {
    let mask = alignment.checked_sub(1).ok_or(LayoutError::Overflow)?;
    if !alignment.is_power_of_two() {
        return Err(LayoutError::Overflow);
    }
    value
        .checked_add(mask)
        .map(|sum| sum & !mask)
        .ok_or(LayoutError::Overflow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::execution_schema::{Architecture, Endianness, RuntimeRep, TargetDescriptor};

    fn target() -> TargetDescriptor {
        TargetDescriptor {
            architecture: Architecture::X86_64,
            endianness: Endianness::Little,
            pointer_width: 64,
            word_width: 64,
            abi: "system-v".into(),
            features: Vec::new(),
        }
    }

    #[test]
    fn descriptor_trace_moves_only_managed_reference_slots() {
        let reps = [
            RuntimeRep::Void,
            RuntimeRep::Word(8),
            RuntimeRep::LiftedRef,
            RuntimeRep::Float(64),
            RuntimeRep::UnliftedRef,
            RuntimeRep::Address,
        ];
        let descriptor = ObjectDescriptor::new(
            ObjectKind::Constructor,
            StorageLayout::for_reps(&target(), &reps).unwrap(),
            None,
        )
        .unwrap();
        let mut owner = vec![0u64; (descriptor.allocation_extent() as usize).div_ceil(8)];
        let mut from = vec![0u64; (layout::LIT_SIZE * 2).div_ceil(8)];
        let mut to = vec![0u8; layout::LIT_SIZE * 2];
        let owner_ptr = owner.as_mut_ptr() as *mut u8;
        let owner_len = std::mem::size_of_val(owner.as_slice());
        let from_ptr = from.as_mut_ptr() as *mut u8;
        let from_len = std::mem::size_of_val(from.as_slice());
        unsafe {
            descriptor.initialize_header(owner_ptr);
            layout::write_header(from_ptr, layout::TAG_LIT, layout::LIT_SIZE as u32);
            layout::write_header(
                from_ptr.add(layout::LIT_SIZE),
                layout::TAG_LIT,
                layout::LIT_SIZE as u32,
            );
            let first = owner_ptr.add(descriptor.trace_offsets()[0] as usize) as *mut *mut u8;
            let second = owner_ptr.add(descriptor.trace_offsets()[1] as usize) as *mut *mut u8;
            *first = from_ptr;
            *second = from_ptr.add(layout::LIT_SIZE);
            let address_field = owner_ptr.add(40) as *mut u64;
            let pointer_looking_bits = from_ptr as u64;
            *address_field = pointer_looking_bits;

            let mut roots = Vec::new();
            descriptor
                .for_each_trace_slot(owner_ptr, owner_len, |slot| roots.push(slot))
                .unwrap();
            assert_eq!(roots, vec![first, second]);
            let result =
                crate::gc::raw::cheney_copy(&roots, from_ptr, from_ptr.add(from_len), &mut to);
            assert_eq!(result.bytes_copied, layout::LIT_SIZE * 2);
            assert_eq!(*first, to.as_mut_ptr());
            assert_eq!(*second, to.as_mut_ptr().add(layout::LIT_SIZE));
            assert_eq!(*address_field, pointer_looking_bits);
        }
    }

    #[test]
    fn descriptor_trace_refuses_truncated_object_before_visiting_slots() {
        let descriptor = ObjectDescriptor::new(
            ObjectKind::Constructor,
            StorageLayout::for_reps(&target(), &[RuntimeRep::LiftedRef]).unwrap(),
            None,
        )
        .unwrap();
        let mut object = vec![0u8; descriptor.allocation_extent() as usize];
        let mut visited = false;
        let error = unsafe {
            descriptor
                .for_each_trace_slot(object.as_mut_ptr(), object.len() - 1, |_| visited = true)
        }
        .unwrap_err();
        assert!(!visited);
        assert!(matches!(error, DescriptorTraceError::Truncated { .. }));
    }
}
