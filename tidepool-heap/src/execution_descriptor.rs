use tidepool_repr::execution_schema::{LayoutError, Signature, StorageLayout};

/// Prepared objects use a descriptor pointer plus low-bit state in one word.
/// The original descriptor survives forwarding, so allocation extent never
/// depends on the object's current contents.
pub const DESCRIPTOR_HEADER_SIZE: u32 = 8;
pub const FORWARDING_POINTER_OFFSET: usize = 8;
const HEADER_STATE_MASK: usize = 7;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(usize)]
pub enum DescriptorState {
    Live = 0,
    Forwarded = 1,
}

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
#[repr(align(8))]
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
    #[error("object header names descriptor {actual:#x}, expected {expected:#x}")]
    HeaderIdentity { expected: usize, actual: usize },
    #[error("invalid prepared object header state {state}")]
    HeaderState { state: usize },
    #[error("forwarded object cannot be used as a live tracing snapshot")]
    ForwardedObject,
    #[error("managed pointer {address:#x} is not a registered object start")]
    UnregisteredPointer { address: usize },
    #[error("object at {address:#x} is not aligned to {alignment} bytes")]
    Misaligned { address: usize, alignment: u32 },
    #[error("copy requires {required} bytes but destination contains {available}")]
    InsufficientSpace { required: usize, available: usize },
    #[error("descriptor metadata allocation failed")]
    MetadataAllocation,
    #[error("copy source/destination ranges are invalid or overlap")]
    InvalidRange,
    #[error("registered object extents overlap at {address:#x}")]
    OverlappingObjects { address: usize },
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
        let header_size = DESCRIPTOR_HEADER_SIZE;
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

    /// Word emitted by both the native allocator and host marshalling.
    /// Its address is valid only while this descriptor remains pinned and owned.
    pub fn initial_header_word(&self) -> usize {
        self as *const Self as usize
    }

    /// Initialize the prepared collector-owned header from this descriptor.
    /// Payload initialization remains the caller's responsibility and must not
    /// begin unless allocation succeeded.
    ///
    /// # Safety
    ///
    /// `ptr` must name writable storage of at least `allocation_extent` bytes.
    /// This descriptor must remain at a stable address, owned for the entire
    /// lifetime of the object and every relocated copy (normally via `Arc`).
    pub unsafe fn initialize_header(&self, ptr: *mut u8) {
        std::ptr::write_unaligned(ptr.cast::<usize>(), self.initial_header_word());
    }

    /// Inspect a header against known, owned descriptor metadata. Never
    /// dereference the untrusted descriptor address read from object bytes.
    ///
    /// # Safety
    /// `ptr` is readable for `available` bytes.
    pub unsafe fn state(
        &self,
        ptr: *const u8,
        available: usize,
    ) -> Result<DescriptorState, DescriptorTraceError> {
        if available < self.allocation_extent as usize {
            return Err(DescriptorTraceError::Truncated {
                declared: self.allocation_extent,
                available,
            });
        }
        let word = std::ptr::read_unaligned(ptr.cast::<usize>());
        let actual = word & !HEADER_STATE_MASK;
        let expected = self.initial_header_word();
        if actual != expected {
            return Err(DescriptorTraceError::HeaderIdentity { expected, actual });
        }
        match word & HEADER_STATE_MASK {
            0 => Ok(DescriptorState::Live),
            1 => Ok(DescriptorState::Forwarded),
            state => Err(DescriptorTraceError::HeaderState { state }),
        }
    }

    /// Install forwarding during the non-fallible commit phase of collection.
    /// Original allocation extent remains in the retained descriptor.
    ///
    /// # Safety
    /// Source is a validated live allocation described by `self`, at least two
    /// words long; destination is its fully initialized relocated copy. No
    /// concurrent observer or collector may inspect this transition.
    pub unsafe fn install_forwarding(&self, source: *mut u8, destination: *mut u8) {
        std::ptr::write_unaligned(
            source.add(FORWARDING_POINTER_OFFSET).cast::<*mut u8>(),
            destination,
        );
        std::ptr::write_unaligned(
            source.cast::<usize>(),
            self.initial_header_word() | DescriptorState::Forwarded as usize,
        );
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
        if self.state(ptr, available)? == DescriptorState::Forwarded {
            return Err(DescriptorTraceError::ForwardedObject);
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
    use crate::gc::raw::{cheney_copy_registered, DescriptorRegistry};
    use crate::layout;
    use std::sync::Arc;
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

    #[repr(align(16))]
    struct Arena([u8; 256]);

    #[test]
    fn compact_header_checks_identity_and_retains_extent_after_forwarding() {
        let descriptor = ObjectDescriptor::new(
            ObjectKind::Thunk,
            StorageLayout::for_reps(&target(), &[RuntimeRep::LiftedRef; 8]).unwrap(),
            None,
        )
        .unwrap();
        let other = ObjectDescriptor::new(
            ObjectKind::Thunk,
            StorageLayout::for_reps(&target(), &[]).unwrap(),
            None,
        )
        .unwrap();
        let mut source = Arena([0; 256]);
        let mut destination = Arena([0; 256]);
        let pointer = source.0.as_mut_ptr();
        let extent = descriptor.allocation_extent();
        unsafe {
            descriptor.initialize_header(pointer);
            assert_eq!(
                descriptor.state(pointer, source.0.len()).unwrap(),
                DescriptorState::Live
            );
            assert!(matches!(
                other.state(pointer, source.0.len()),
                Err(DescriptorTraceError::HeaderIdentity { .. })
            ));
            std::ptr::copy_nonoverlapping(pointer, destination.0.as_mut_ptr(), extent as usize);
            descriptor.install_forwarding(pointer, destination.0.as_mut_ptr());
            assert_eq!(
                descriptor.state(pointer, source.0.len()).unwrap(),
                DescriptorState::Forwarded
            );
            assert_eq!(
                descriptor
                    .state(destination.0.as_ptr(), destination.0.len())
                    .unwrap(),
                DescriptorState::Live
            );
            assert_eq!(descriptor.allocation_extent(), extent);
            assert_eq!(
                std::ptr::read_unaligned(pointer.add(FORWARDING_POINTER_OFFSET).cast::<*mut u8>()),
                destination.0.as_mut_ptr()
            );
            assert!(matches!(
                descriptor.for_each_trace_slot(pointer, source.0.len(), |_| panic!(
                    "forwarded payload visited"
                )),
                Err(DescriptorTraceError::ForwardedObject)
            ));
        }
    }

    #[test]
    fn registered_copy_rejects_partial_source_and_occupied_destination() {
        let descriptor = Arc::new(
            ObjectDescriptor::new(
                ObjectKind::Constructor,
                StorageLayout::for_reps(&target(), &[]).unwrap(),
                None,
            )
            .unwrap(),
        );
        let mut source = Arena([0; 256]);
        let mut destination = Arena([0; 256]);
        let pointer = source.0.as_mut_ptr();
        let mut registry = DescriptorRegistry::new();
        unsafe {
            descriptor.initialize_header(pointer);
            registry
                .register(pointer, source.0.len(), Arc::clone(&descriptor))
                .unwrap();
        }
        let before = source.0;
        let error = unsafe {
            cheney_copy_registered(
                &[],
                pointer.add(8),
                pointer.add(16),
                &mut destination.0,
                &mut registry,
            )
        }
        .err()
        .expect("partially included allocation must fail");
        assert!(matches!(error, DescriptorTraceError::InvalidRange));
        assert_eq!(source.0, before);
        unsafe {
            descriptor.initialize_header(destination.0.as_mut_ptr());
            registry
                .register(
                    destination.0.as_mut_ptr(),
                    destination.0.len(),
                    Arc::clone(&descriptor),
                )
                .unwrap();
        }
        let destination_before = destination.0;
        let error = unsafe {
            cheney_copy_registered(
                &[],
                pointer,
                pointer.add(16),
                &mut destination.0,
                &mut registry,
            )
        }
        .err()
        .expect("occupied destination must fail");
        assert!(matches!(error, DescriptorTraceError::InvalidRange));
        assert_eq!(source.0, before);
        assert_eq!(destination.0, destination_before);
    }

    #[test]
    fn registered_copy_capacity_failure_preserves_source_roots_and_destination() {
        let descriptor = Arc::new(
            ObjectDescriptor::new(
                ObjectKind::Constructor,
                StorageLayout::for_reps(&target(), &[RuntimeRep::Int(128)]).unwrap(),
                None,
            )
            .unwrap(),
        );
        let mut source = Arena([0; 256]);
        let mut destination = Arena([0x5a; 256]);
        let pointer = source.0.as_mut_ptr();
        let extent = descriptor.allocation_extent() as usize;
        let mut registry = DescriptorRegistry::new();
        unsafe {
            descriptor.initialize_header(pointer);
            registry
                .register(pointer, extent, Arc::clone(&descriptor))
                .unwrap();
        }
        let before = source.0;
        let mut root = pointer;
        let error = unsafe {
            cheney_copy_registered(
                &[&mut root],
                pointer,
                pointer.add(extent),
                &mut destination.0[..extent - 1],
                &mut registry,
            )
        }
        .err()
        .expect("insufficient destination must fail");
        assert!(matches!(
            error,
            DescriptorTraceError::InsufficientSpace { .. }
        ));
        assert_eq!(root, pointer);
        assert_eq!(source.0, before);
        assert_eq!(destination.0, [0x5a; 256]);
        assert!(registry.descriptor(pointer).is_some());
    }

    #[test]
    fn registered_copy_rejects_late_interior_child_before_any_mutation() {
        let owner = Arc::new(
            ObjectDescriptor::new(
                ObjectKind::Constructor,
                StorageLayout::for_reps(&target(), &[RuntimeRep::LiftedRef, RuntimeRep::LiftedRef])
                    .unwrap(),
                None,
            )
            .unwrap(),
        );
        let child = Arc::new(
            ObjectDescriptor::new(
                ObjectKind::Constructor,
                StorageLayout::for_reps(&target(), &[]).unwrap(),
                None,
            )
            .unwrap(),
        );
        let mut source = Arena([0; 256]);
        let mut destination = Arena([0x5a; 256]);
        let pointer = source.0.as_mut_ptr();
        let owner_extent = owner.allocation_extent() as usize;
        let child_extent = child.allocation_extent() as usize;
        let child_pointer = unsafe { pointer.add(owner_extent) };
        let mut registry = DescriptorRegistry::new();
        unsafe {
            owner.initialize_header(pointer);
            child.initialize_header(child_pointer);
            std::ptr::write(
                pointer
                    .add(owner.trace_offsets()[0] as usize)
                    .cast::<*mut u8>(),
                child_pointer,
            );
            std::ptr::write(
                pointer
                    .add(owner.trace_offsets()[1] as usize)
                    .cast::<*mut u8>(),
                child_pointer.add(8),
            );
            registry
                .register(pointer, owner_extent, Arc::clone(&owner))
                .unwrap();
            registry
                .register(child_pointer, child_extent, Arc::clone(&child))
                .unwrap();
        }
        let before = source.0;
        let mut root = pointer;
        let error = unsafe {
            cheney_copy_registered(
                &[&mut root],
                pointer,
                pointer.add(owner_extent + child_extent),
                &mut destination.0,
                &mut registry,
            )
        }
        .err()
        .expect("interior reference must fail");
        assert!(matches!(
            error,
            DescriptorTraceError::UnregisteredPointer { .. }
        ));
        assert_eq!(root, pointer);
        assert_eq!(source.0, before);
        assert_eq!(destination.0, [0x5a; 256]);
        assert!(registry.descriptor(pointer).is_some());
        assert!(registry.descriptor(child_pointer).is_some());
    }

    #[test]
    fn registered_copy_aligns_each_mixed_layout_destination() {
        let narrow = Arc::new(
            ObjectDescriptor::new(
                ObjectKind::Constructor,
                StorageLayout::for_reps(&target(), &[RuntimeRep::Word(64), RuntimeRep::Word(64)])
                    .unwrap(),
                None,
            )
            .unwrap(),
        );
        let wide = Arc::new(
            ObjectDescriptor::new(
                ObjectKind::Constructor,
                StorageLayout::for_reps(&target(), &[RuntimeRep::Int(128)]).unwrap(),
                None,
            )
            .unwrap(),
        );
        let mut source = Arena([0; 256]);
        let mut destination = Arena([0; 256]);
        let pointer = source.0.as_mut_ptr();
        let wide_pointer = unsafe { pointer.add(32) };
        let mut registry = DescriptorRegistry::new();
        unsafe {
            narrow.initialize_header(pointer);
            wide.initialize_header(wide_pointer);
            registry
                .register(
                    pointer,
                    narrow.allocation_extent() as usize,
                    Arc::clone(&narrow),
                )
                .unwrap();
            registry
                .register(
                    wide_pointer,
                    wide.allocation_extent() as usize,
                    Arc::clone(&wide),
                )
                .unwrap();
        }
        let mut first = pointer;
        let mut second = wide_pointer;
        let copied = unsafe {
            cheney_copy_registered(
                &[&mut first, &mut second],
                pointer,
                pointer.add(64),
                &mut destination.0[1..],
                &mut registry,
            )
            .unwrap()
        };
        assert_eq!(first as usize % narrow.allocation_alignment() as usize, 0);
        assert_eq!(second as usize % wide.allocation_alignment() as usize, 0);
        assert!(second as usize >= first as usize + narrow.allocation_extent() as usize);
        assert_eq!(
            copied.bytes_copied,
            second as usize - destination.0[1..].as_ptr() as usize
                + wide.allocation_extent() as usize
        );
        assert!(registry.descriptor(first).is_some());
        assert!(registry.descriptor(second).is_some());
    }
}
