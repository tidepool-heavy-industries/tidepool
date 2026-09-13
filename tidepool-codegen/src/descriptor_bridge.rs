use std::ptr;

use tidepool_heap::execution_descriptor::ObjectDescriptor;
use tidepool_repr::execution_schema::RuntimeRep;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DescriptorValue {
    Void,
    Managed(*mut u8),
    Address(*const u8),
    /// Target-encoded scalar bytes. Only the field's declared width is copied.
    Bits([u8; 16]),
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum DescriptorMarshalError {
    #[error("descriptor needs {required} bytes, only {available} available")]
    InsufficientStorage { required: usize, available: usize },
    #[error("descriptor expects {expected} semantic values, received {actual}")]
    ValueCount { expected: usize, actual: usize },
    #[error("value {index} does not match representation {rep:?}")]
    Representation { index: usize, rep: RuntimeRep },
    #[error("descriptor pointer field width {0} is not native pointer width")]
    PointerWidth(u32),
}

/// Initialize and marshal one descriptor-owned object.
///
/// All values and bounds are checked before the header is written, so a failed
/// call never publishes a partially initialized object to the collector.
/// Scalar bytes are already target encoded by the caller; this function does
/// not duplicate representation or endianness policy.
///
/// # Safety
///
/// `object` must name `available` writable bytes and must not become reachable
/// or be collected until this function returns successfully. `descriptor` must
/// remain pinned and owned for the full lifetime of this object and every
/// relocated copy: the initialized header stores its address.
pub unsafe fn marshal_descriptor_object(
    object: *mut u8,
    available: usize,
    descriptor: &ObjectDescriptor,
    values: &[DescriptorValue],
) -> Result<(), DescriptorMarshalError> {
    let required = descriptor.allocation_extent() as usize;
    if available < required {
        return Err(DescriptorMarshalError::InsufficientStorage {
            required,
            available,
        });
    }
    let logical = descriptor.payload().logical_to_stored();
    if values.len() != logical.len() {
        return Err(DescriptorMarshalError::ValueCount {
            expected: logical.len(),
            actual: values.len(),
        });
    }

    for (index, (value, stored)) in values.iter().zip(logical).enumerate() {
        let rep = stored
            .and_then(|stored| descriptor.payload().fields().get(stored as usize))
            .map_or(RuntimeRep::Void, |field| field.rep());
        validate_value(index, rep, *value, descriptor)?;
    }

    descriptor.initialize_header(object);
    for (value, stored) in values.iter().zip(logical) {
        let Some(field) =
            stored.and_then(|index| descriptor.payload().fields().get(index as usize))
        else {
            continue;
        };
        let destination = object.add(descriptor.payload_base() as usize + field.offset() as usize);
        match value {
            DescriptorValue::Void => unreachable!("validated stored field cannot be Void"),
            DescriptorValue::Managed(value) => {
                ptr::copy_nonoverlapping(
                    (*value as usize).to_ne_bytes().as_ptr(),
                    destination,
                    field.size() as usize,
                );
            }
            DescriptorValue::Address(value) => {
                ptr::copy_nonoverlapping(
                    (*value as usize).to_ne_bytes().as_ptr(),
                    destination,
                    field.size() as usize,
                );
            }
            DescriptorValue::Bits(bytes) => {
                ptr::copy_nonoverlapping(bytes.as_ptr(), destination, field.size() as usize);
            }
        }
    }
    Ok(())
}

fn validate_value(
    index: usize,
    rep: RuntimeRep,
    value: DescriptorValue,
    descriptor: &ObjectDescriptor,
) -> Result<(), DescriptorMarshalError> {
    let matches = matches!(
        (rep, value),
        (RuntimeRep::Void, DescriptorValue::Void)
            | (
                RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef,
                DescriptorValue::Managed(_)
            )
            | (RuntimeRep::Address, DescriptorValue::Address(_))
            | (
                RuntimeRep::Int(_) | RuntimeRep::Word(_) | RuntimeRep::Float(_),
                DescriptorValue::Bits(_)
            )
    );
    if !matches {
        return Err(DescriptorMarshalError::Representation { index, rep });
    }
    if matches!(
        rep,
        RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef | RuntimeRep::Address
    ) && descriptor
        .payload()
        .logical_to_stored()
        .get(index)
        .and_then(|stored| *stored)
        .and_then(|stored| descriptor.payload().fields().get(stored as usize))
        .is_some_and(|field| field.size() as usize != size_of::<usize>())
    {
        return Err(DescriptorMarshalError::PointerWidth(
            descriptor.payload().logical_to_stored()[index]
                .and_then(|stored| descriptor.payload().fields().get(stored as usize))
                .map_or(0, |field| field.size()),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tidepool_heap::execution_descriptor::{
        DescriptorState, EntryMetadata, ObjectKind, FORWARDING_POINTER_OFFSET,
    };
    use tidepool_heap::gc::raw::{cheney_copy_registered, DescriptorRegistry};
    use tidepool_repr::execution_schema::{
        Architecture, Endianness, Signature, StorageLayout, TargetDescriptor,
    };

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

    fn bits(value: u128) -> DescriptorValue {
        DescriptorValue::Bits(value.to_le_bytes())
    }

    #[test]
    fn descriptor_marshalling_omits_void_preserves_extent_and_checks_before_header() {
        let reps = vec![
            RuntimeRep::Void,
            RuntimeRep::Word(8),
            RuntimeRep::LiftedRef,
            RuntimeRep::Float(64),
            RuntimeRep::UnliftedRef,
            RuntimeRep::Address,
        ];
        let descriptor = ObjectDescriptor::new(
            ObjectKind::Pap,
            StorageLayout::for_reps(&target(), &reps).unwrap(),
            Some(EntryMetadata::new(
                Signature {
                    arguments: reps,
                    results: vec![RuntimeRep::LiftedRef],
                },
                11,
            )),
        )
        .unwrap();
        let mut live_a = 1u64;
        let mut live_b = 2u64;
        let values = [
            DescriptorValue::Void,
            bits(7),
            DescriptorValue::Managed((&mut live_a as *mut u64).cast()),
            bits(f64::to_bits(3.5).into()),
            DescriptorValue::Managed((&mut live_b as *mut u64).cast()),
            DescriptorValue::Address(0xfeedusize as *const u8),
        ];
        let mut bytes = vec![0u8; descriptor.allocation_extent() as usize];
        let extent = descriptor.allocation_extent();
        unsafe {
            marshal_descriptor_object(bytes.as_mut_ptr(), bytes.len(), &descriptor, &values)
                .unwrap();
            assert_eq!(
                descriptor.state(bytes.as_ptr(), bytes.len()).unwrap(),
                DescriptorState::Live
            );
            assert_eq!(
                ptr::read_unaligned(bytes.as_ptr().cast::<usize>()),
                descriptor.initial_header_word()
            );
            assert_eq!(descriptor.allocation_extent(), extent);
            assert_eq!(descriptor.trace_offsets(), &[16, 32]);
            assert_eq!(bytes[descriptor.payload_base() as usize], 7);

            let mut relocated = bytes.clone();
            descriptor.install_forwarding(bytes.as_mut_ptr(), relocated.as_mut_ptr());
            assert_eq!(
                descriptor.state(bytes.as_ptr(), bytes.len()).unwrap(),
                DescriptorState::Forwarded
            );
            assert_eq!(
                descriptor
                    .state(relocated.as_ptr(), relocated.len())
                    .unwrap(),
                DescriptorState::Live
            );
            assert_eq!(
                ptr::read_unaligned(
                    bytes
                        .as_ptr()
                        .add(FORWARDING_POINTER_OFFSET)
                        .cast::<*mut u8>()
                ),
                relocated.as_mut_ptr()
            );
            assert_eq!(descriptor.allocation_extent(), extent);
        }

        let mut rejected = vec![0u8; descriptor.allocation_extent() as usize];
        let mut wrong = values;
        wrong[2] = DescriptorValue::Address(ptr::null());
        let error = unsafe {
            marshal_descriptor_object(rejected.as_mut_ptr(), rejected.len(), &descriptor, &wrong)
        }
        .unwrap_err();
        assert!(matches!(
            error,
            DescriptorMarshalError::Representation {
                index: 2,
                rep: RuntimeRep::LiftedRef
            }
        ));
        assert_eq!(rejected[0], 0, "failed validation published a header");
    }

    #[test]
    fn descriptor_honors_sixteen_byte_payload_alignment() {
        let descriptor = ObjectDescriptor::new(
            ObjectKind::Constructor,
            StorageLayout::for_reps(&target(), &[RuntimeRep::Int(128)]).unwrap(),
            None,
        )
        .unwrap();
        assert_eq!(descriptor.payload_base(), 16);
        assert_eq!(descriptor.allocation_alignment(), 16);
        assert_eq!(descriptor.allocation_extent(), 32);
    }

    #[test]
    fn descriptor_kinds_publish_owned_header_identity_and_live_state() {
        let empty = StorageLayout::for_reps(&target(), &[]).unwrap();
        let entry = || {
            Some(EntryMetadata::new(
                Signature {
                    arguments: Vec::new(),
                    results: vec![RuntimeRep::LiftedRef],
                },
                19,
            ))
        };
        let cases = [
            (ObjectKind::Function, entry()),
            (ObjectKind::Pap, entry()),
            (ObjectKind::Thunk, entry()),
            (ObjectKind::Continuation, entry()),
            (ObjectKind::Constructor, None),
        ];
        for (kind, metadata) in cases {
            let descriptor = ObjectDescriptor::new(kind, empty.clone(), metadata).unwrap();
            let mut bytes = vec![0u8; descriptor.allocation_extent() as usize];
            let extent = descriptor.allocation_extent();
            unsafe {
                descriptor.initialize_header(bytes.as_mut_ptr());
                assert_eq!(
                    ptr::read_unaligned(bytes.as_ptr().cast::<usize>()),
                    descriptor.initial_header_word()
                );
                assert_eq!(
                    descriptor.state(bytes.as_ptr(), bytes.len()).unwrap(),
                    DescriptorState::Live
                );
                assert_eq!(descriptor.allocation_extent(), extent);
            }
        }
    }

    #[test]
    fn registered_descriptor_object_survives_copy_and_traces_only_managed_slots() {
        let descriptor = Arc::new(
            ObjectDescriptor::new(
                ObjectKind::Constructor,
                StorageLayout::for_reps(
                    &target(),
                    &[
                        RuntimeRep::LiftedRef,
                        RuntimeRep::Address,
                        RuntimeRep::UnliftedRef,
                    ],
                )
                .unwrap(),
                None,
            )
            .unwrap(),
        );
        let scalar_descriptor = Arc::new(
            ObjectDescriptor::new(
                ObjectKind::Constructor,
                StorageLayout::for_reps(&target(), &[RuntimeRep::Word(64)]).unwrap(),
                None,
            )
            .unwrap(),
        );
        let owner_size = descriptor.allocation_extent() as usize;
        let child_offset = owner_size;
        let child_size = scalar_descriptor.allocation_extent() as usize;
        let from_len = owner_size + child_size;
        let mut from = vec![0u64; from_len.div_ceil(size_of::<u64>())];
        let mut to = vec![0u8; from.len() * size_of::<u64>()];
        let from_ptr = from.as_mut_ptr().cast::<u8>();
        let actual_from_len = from.len() * size_of::<u64>();
        let child = unsafe { from_ptr.add(child_offset) };
        unsafe {
            marshal_descriptor_object(child, child_size, &scalar_descriptor, &[bits(41)]).unwrap();
        }
        let values = [
            DescriptorValue::Managed(child),
            DescriptorValue::Address(child),
            DescriptorValue::Managed(child),
        ];
        unsafe {
            marshal_descriptor_object(from_ptr, owner_size, &descriptor, &values).unwrap();
        }

        let mut registry = DescriptorRegistry::new();
        unsafe {
            registry
                .register(from_ptr, owner_size, Arc::clone(&descriptor))
                .unwrap();
            registry
                .register(child, child_size, Arc::clone(&scalar_descriptor))
                .unwrap();
        }
        let mut root = from_ptr;
        let roots = [&mut root as *mut *mut u8];
        let copied = unsafe {
            cheney_copy_registered(
                &roots,
                from_ptr,
                from_ptr.add(actual_from_len),
                &mut to,
                &mut registry,
            )
            .unwrap()
        };

        assert_eq!(copied.bytes_copied, owner_size + child_size);
        assert_eq!(root, to.as_mut_ptr());
        assert!(registry.descriptor(root).is_some());
        assert!(registry.descriptor(from_ptr).is_none());
        unsafe {
            let first = root.add(descriptor.trace_offsets()[0] as usize) as *const *mut u8;
            let address = root.add(descriptor.payload_base() as usize + 8) as *const *mut u8;
            let second = root.add(descriptor.trace_offsets()[1] as usize) as *const *mut u8;
            let moved_child = to.as_mut_ptr().add(owner_size);
            assert_eq!(*first, moved_child);
            assert_eq!(*second, moved_child);
            assert_eq!(*address, child, "Address values are not GC roots");
            assert!(registry.descriptor(moved_child).is_some());
            assert!(registry.descriptor(child).is_none());
        }
    }

    #[test]
    fn registered_collection_refuses_truncated_range_before_forwarding() {
        let descriptor = Arc::new(
            ObjectDescriptor::new(
                ObjectKind::Constructor,
                StorageLayout::for_reps(&target(), &[RuntimeRep::LiftedRef]).unwrap(),
                None,
            )
            .unwrap(),
        );
        let extent = descriptor.allocation_extent() as usize;
        let mut from = vec![0u64; extent.div_ceil(size_of::<u64>())];
        let from_ptr = from.as_mut_ptr().cast::<u8>();
        unsafe {
            marshal_descriptor_object(
                from_ptr,
                extent,
                &descriptor,
                &[DescriptorValue::Managed(ptr::null_mut())],
            )
            .unwrap();
        }
        let mut registry = DescriptorRegistry::new();
        unsafe {
            registry
                .register(from_ptr, extent, Arc::clone(&descriptor))
                .unwrap();
        }
        let mut root = from_ptr;
        let roots = [&mut root as *mut *mut u8];
        let mut to = vec![0u8; extent];
        let error = match unsafe {
            cheney_copy_registered(
                &roots,
                from_ptr,
                from_ptr.add(extent - 1),
                &mut to,
                &mut registry,
            )
        } {
            Ok(_) => panic!("truncated registered collection succeeded"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            tidepool_heap::execution_descriptor::DescriptorTraceError::Truncated { .. }
        ));
        assert_eq!(root, from_ptr);
        assert_eq!(
            unsafe { descriptor.state(from_ptr, extent) }.unwrap(),
            DescriptorState::Live
        );
        assert!(registry.descriptor(from_ptr).is_some());
    }

    #[test]
    fn registered_collection_retires_unreachable_descriptors() {
        let descriptor = Arc::new(
            ObjectDescriptor::new(
                ObjectKind::Constructor,
                StorageLayout::for_reps(&target(), &[]).unwrap(),
                None,
            )
            .unwrap(),
        );
        let extent = descriptor.allocation_extent() as usize;
        let mut from = vec![0u64; (extent * 2).div_ceil(size_of::<u64>())];
        let from_ptr = from.as_mut_ptr().cast::<u8>();
        let dead = unsafe { from_ptr.add(extent) };
        unsafe {
            descriptor.initialize_header(from_ptr);
            descriptor.initialize_header(dead);
        }
        let mut registry = DescriptorRegistry::new();
        unsafe {
            registry
                .register(from_ptr, extent, Arc::clone(&descriptor))
                .unwrap();
            registry
                .register(dead, extent, Arc::clone(&descriptor))
                .unwrap();
        }
        let mut root = from_ptr;
        let roots = [&mut root as *mut *mut u8];
        let mut to = vec![0u8; extent * 2];
        let result = unsafe {
            cheney_copy_registered(
                &roots,
                from_ptr,
                from_ptr.add(extent * 2),
                &mut to,
                &mut registry,
            )
            .unwrap()
        };

        assert_eq!(result.bytes_copied, extent);
        assert!(registry.descriptor(root).is_some());
        assert!(registry.descriptor(from_ptr).is_none());
        assert!(registry.descriptor(dead).is_none());
    }
}
