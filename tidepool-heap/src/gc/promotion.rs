//! Selective promotion and mandatory sibling fixup form one no-mutator interval.
//!
//! The raw collector owns both the exact source walk and Cheney graph copier.
//! Preparation rejects preexisting Forwarded headers and reserves both copies'
//! scratch before mutation. Copying reuses that authenticated source map; only
//! this operation may introduce forwarding between promotion and sibling fixup.

use super::raw::{self, DescriptorSpace};
use crate::descriptor_region::{DescriptorArena, DescriptorOldSpace};
use crate::execution_descriptor::DescriptorTraceError;
use crate::external_storage::{ExternalPayloadOwner, ExternalStorageKind};

#[derive(Debug)]
pub struct PromotionResult {
    pub promoted_bytes: usize,
    pub nursery_bytes: usize,
    /// Payloads expanded while copying the selected graph. Sibling fixup
    /// deliberately has a fresh scratch set and is not included here.
    pub promoted_external_payloads: Vec<(usize, ExternalStorageKind)>,
}

/// Preparation has not changed objects. Incomplete means roots or either heap
/// may already have changed: retain all owners and permanently retire the
/// invocation, even if the underlying cause is ordinarily a resource failure.
#[derive(Debug, thiserror::Error)]
pub enum PromotionFailure {
    #[error("promotion preparation: {0}")]
    Preparation(DescriptorTraceError),
    #[error("incomplete promotion; invocation must retire: {0}")]
    Incomplete(DescriptorTraceError),
}

struct PromotedAndOld<'a> {
    promoted: &'a DescriptorArena,
    previous: Option<&'a dyn DescriptorOldSpace>,
}

// SAFETY: both owners remain borrowed throughout fixup; the freshly copied
// region was sealed before this view is constructed. No mutator can run here.
unsafe impl DescriptorOldSpace for PromotedAndOld<'_> {
    fn admit(&self, encoded: usize) -> Result<Option<usize>, DescriptorTraceError> {
        match self.promoted.admit(encoded)? {
            Some(reference) => Ok(Some(reference)),
            None => self.previous.map_or(Ok(None), |owner| owner.admit(encoded)),
        }
    }
}

/// # Safety
/// Source, destination, spare nursery and root slots are disjoint, initialized
/// and owned through success/error/native unwind. `selected` slots occur in
/// `all_roots`, a complete machine snapshot after generated frames have unwound.
/// OldSpace has installed destination ownership BEFORE calling. On success the
/// caller publishes the spare nursery immediately, without a fallible step.
/// On Incomplete it must not execute, observe, or discard either live space.
/// Cancellation is intentionally absent inside this operation.
#[allow(
    clippy::too_many_arguments,
    reason = "the unsafe promotion boundary keeps independent spaces, root sets, descriptors, and old-space ownership explicit"
)]
pub unsafe fn promote_and_fixup(
    selected: &[*mut *mut u8],
    all_roots: &[*mut *mut u8],
    from_start: *const u8,
    from_used: usize,
    nursery_spare: &mut [u8],
    destination: &mut DescriptorArena,
    descriptors: &mut DescriptorSpace,
    previous: Option<&dyn DescriptorOldSpace>,
) -> Result<PromotionResult, PromotionFailure> {
    promote_and_fixup_inner(
        selected,
        all_roots,
        from_start,
        from_used,
        nursery_spare,
        destination,
        descriptors,
        previous,
        None,
    )
}

/// Selectively promote with authenticated external payload edges. This keeps
/// the existing promotion/fixup protocol and only extends each Cheney copy
/// phase with the owner-provided bounded slots.
///
/// # Safety
/// All requirements of [`promote_and_fixup`] apply. `external` must satisfy
/// [`ExternalPayloadOwner`]'s authentication and exclusivity contract, and
/// every returned slot must remain allocated, initialized, and exclusively
/// available to the collector through both Cheney copies and native unwind.
#[allow(
    clippy::too_many_arguments,
    reason = "the unsafe promotion boundary keeps independent spaces, root sets, descriptors, old-space, and external ownership explicit"
)]
pub unsafe fn promote_and_fixup_with_external(
    selected: &[*mut *mut u8],
    all_roots: &[*mut *mut u8],
    from_start: *const u8,
    from_used: usize,
    nursery_spare: &mut [u8],
    destination: &mut DescriptorArena,
    descriptors: &mut DescriptorSpace,
    previous: Option<&dyn DescriptorOldSpace>,
    external: &dyn ExternalPayloadOwner,
) -> Result<PromotionResult, PromotionFailure> {
    promote_and_fixup_inner(
        selected,
        all_roots,
        from_start,
        from_used,
        nursery_spare,
        destination,
        descriptors,
        previous,
        Some(external),
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the unsafe promotion boundary keeps independent spaces, root sets, descriptors, old-space, and optional external ownership explicit"
)]
unsafe fn promote_and_fixup_inner(
    selected: &[*mut *mut u8],
    all_roots: &[*mut *mut u8],
    from_start: *const u8,
    from_used: usize,
    nursery_spare: &mut [u8],
    destination: &mut DescriptorArena,
    descriptors: &mut DescriptorSpace,
    previous: Option<&dyn DescriptorOldSpace>,
    external: Option<&dyn ExternalPayloadOwner>,
) -> Result<PromotionResult, PromotionFailure> {
    use PromotionFailure::{Incomplete, Preparation};
    if selected.iter().any(|slot| !all_roots.contains(slot)) {
        return Err(Preparation(DescriptorTraceError::InvalidRange));
    }
    // Both preparations happen before forwarding. The complete snapshot sizes
    // reusable root scratch for either copy. The second source validation still
    // rejects Forwarded, so a caller cannot smuggle a fabricated relocation in.
    raw::prepare_descriptor_copy(
        all_roots,
        from_start,
        from_used,
        destination.destination(),
        descriptors,
    )
    .map_err(Preparation)?;
    raw::prepare_descriptor_copy(all_roots, from_start, from_used, nursery_spare, descriptors)
        .map_err(Preparation)?;

    let promoted = raw::copy_prevalidated_descriptor_graph_with_external(
        selected,
        from_start,
        from_used,
        destination.destination(),
        descriptors,
        previous,
        external,
    )
    .map_err(Incomplete)?;
    let mut promoted_external_payloads = Vec::new();
    let payload_count = descriptors.visited_external_payloads().count();
    promoted_external_payloads
        .try_reserve(payload_count)
        .map_err(|_| Incomplete(DescriptorTraceError::MetadataAllocation))?;
    promoted_external_payloads.extend(descriptors.visited_external_payloads());
    destination
        .seal(promoted.bytes_copied)
        .map_err(Incomplete)?;
    let admitted = PromotedAndOld {
        promoted: destination,
        previous,
    };
    // Source starts/descriptor identities remain those authenticated before
    // promotion. Only this operation has written Forwarded states since then.
    // Updated compression may point at a different descriptor, including an
    // already admitted static/old target; exact-target admission is the proof.
    let nursery = raw::copy_prevalidated_descriptor_graph_with_external(
        all_roots,
        from_start,
        from_used,
        nursery_spare,
        descriptors,
        Some(&admitted),
        external,
    )
    .map_err(Incomplete)?;
    Ok(PromotionResult {
        promoted_bytes: promoted.bytes_copied,
        nursery_bytes: nursery.bytes_copied,
        promoted_external_payloads,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution_descriptor::{DescriptorState, ObjectDescriptor};
    use crate::external_storage::{
        ExternalPointerSlots, ExternalStorageKind, ExternalStorageValidationError,
    };
    use crate::managed_reference::{tag_of, untag};
    use std::rc::Rc;
    use std::sync::Arc;
    use tidepool_repr::execution_schema::{
        Architecture, Endianness, RuntimeRep, StorageLayout, TargetDescriptor,
    };

    fn descriptor() -> Arc<ObjectDescriptor> {
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
                1,
                StorageLayout::for_reps(&target, &[RuntimeRep::LiftedRef]).unwrap(),
                None,
            )
            .unwrap(),
        )
    }

    unsafe fn write_node(
        base: *mut u8,
        offset: usize,
        descriptor: &ObjectDescriptor,
        child: *mut u8,
    ) -> *mut u8 {
        let object = base.add(offset);
        descriptor.initialize_header(object);
        std::ptr::write(
            object.cast::<usize>(),
            descriptor.initial_header_word() | DescriptorState::Live as usize,
        );
        if let Some(&offset) = descriptor.trace_offsets().first() {
            std::ptr::write(object.add(offset as usize).cast(), child);
        }
        object
    }

    unsafe fn write_external(
        base: *mut u8,
        offset: usize,
        descriptor: &ObjectDescriptor,
        published: *mut u8,
    ) -> *mut u8 {
        let object = base.add(offset);
        descriptor.initialize_header(object);
        std::ptr::write(
            object.cast::<usize>(),
            descriptor.initial_header_word() | DescriptorState::Live as usize,
        );
        std::ptr::write(
            object
                .add(descriptor.payload_base() as usize)
                .cast::<*mut u8>(),
            published,
        );
        object
    }

    #[test]
    fn selected_promotion_repairs_unpromoted_sibling_alias() {
        let descriptor = descriptor();
        let extent = descriptor.allocation_extent() as usize;
        let source_bytes = extent * 3;
        let mut source = vec![0_u64; source_bytes / 8];
        let mut spare = vec![0_u64; source_bytes / 8];
        let mut descriptors = DescriptorSpace::new([Arc::clone(&descriptor)]).unwrap();
        let mut destination =
            DescriptorArena::reserve(source_bytes, [Arc::clone(&descriptor)]).unwrap();
        let (a, b, c) = unsafe {
            let base = source.as_mut_ptr().cast::<u8>();
            let c = write_node(base, extent * 2, &descriptor, std::ptr::null_mut());
            let c_tagged = c as usize | usize::from(descriptor.tag());
            let b = write_node(base, extent, &descriptor, c_tagged as *mut u8);
            let a = write_node(base, 0, &descriptor, c_tagged as *mut u8);
            (a, b, c)
        };
        let mut selected_root = a;
        let mut sibling_root = b;
        let all_roots = [
            &mut selected_root as *mut *mut u8,
            &mut sibling_root as *mut *mut u8,
        ];
        let selected = [all_roots[0]];
        let result = unsafe {
            promote_and_fixup(
                &selected,
                &all_roots,
                source.as_ptr().cast(),
                source_bytes,
                std::slice::from_raw_parts_mut(spare.as_mut_ptr().cast(), source_bytes),
                &mut destination,
                &mut descriptors,
                None,
            )
        }
        .unwrap();
        assert_eq!(result.promoted_bytes, extent * 2);
        assert_eq!(result.nursery_bytes, extent);
        let promoted_child = unsafe {
            std::ptr::read(
                selected_root
                    .add(descriptor.trace_offsets()[0] as usize)
                    .cast::<usize>(),
            )
        };
        let sibling_child = unsafe {
            std::ptr::read(
                sibling_root
                    .add(descriptor.trace_offsets()[0] as usize)
                    .cast::<usize>(),
            )
        };
        assert_eq!(untag(promoted_child), untag(sibling_child));
        assert_eq!(
            untag(promoted_child),
            untag(destination.destination().as_ptr() as usize + extent)
        );
        let _ = c;
    }

    #[test]
    fn incomplete_promotion_retains_forwarded_destination_state() {
        let descriptor = descriptor();
        let extent = descriptor.allocation_extent() as usize;
        let source_bytes = extent;
        let mut source = vec![0_u64; source_bytes / 8];
        let mut spare = vec![0_u64; source_bytes / 8];
        let mut descriptors = DescriptorSpace::new([Arc::clone(&descriptor)]).unwrap();
        let mut destination =
            DescriptorArena::reserve(source_bytes, [Arc::clone(&descriptor)]).unwrap();
        let root_object = unsafe {
            let base = source.as_mut_ptr().cast::<u8>();
            write_node(base, 0, &descriptor, (base as usize + 8) as *mut u8)
        };
        let mut root = root_object;
        let roots = [&mut root as *mut *mut u8];
        let failure = unsafe {
            promote_and_fixup(
                &roots,
                &roots,
                source.as_ptr().cast(),
                source_bytes,
                std::slice::from_raw_parts_mut(spare.as_mut_ptr().cast(), source_bytes),
                &mut destination,
                &mut descriptors,
                None,
            )
        }
        .unwrap_err();
        assert!(matches!(failure, PromotionFailure::Incomplete(_)));
        let state = unsafe { descriptor.state(root_object, extent) }.unwrap();
        assert_eq!(state, DescriptorState::Forwarded);
        assert!(!destination.destination().is_empty());
    }

    #[test]
    fn old_target_nursery_field_is_rewritten_during_fixup() {
        let descriptor = descriptor();
        let extent = descriptor.allocation_extent() as usize;
        let mut source = vec![0_u64; extent / 8];
        let mut spare = vec![0_u64; extent / 8];
        let mut descriptors = DescriptorSpace::new([Arc::clone(&descriptor)]).unwrap();
        let mut destination = DescriptorArena::reserve(extent, [Arc::clone(&descriptor)]).unwrap();
        let mut previous = DescriptorArena::reserve(extent, [Arc::clone(&descriptor)]).unwrap();
        let source_object = unsafe {
            write_node(
                source.as_mut_ptr().cast(),
                0,
                &descriptor,
                std::ptr::null_mut(),
            )
        };
        let old_object = unsafe {
            let object = previous.destination().as_mut_ptr();
            descriptor.initialize_header(object);
            std::ptr::write(
                object.cast::<usize>(),
                descriptor.initial_header_word() | DescriptorState::Live as usize,
            );
            std::ptr::write(
                object
                    .add(descriptor.trace_offsets()[0] as usize)
                    .cast::<usize>(),
                source_object as usize | usize::from(descriptor.tag()),
            );
            object
        };
        previous.seal(extent).unwrap();
        let old_field =
            unsafe { old_object.add(descriptor.trace_offsets()[0] as usize) as *mut *mut u8 };
        let mut source_root = source_object;
        let all_roots = [&mut source_root as *mut *mut u8, old_field];
        let selected = [all_roots[0]];
        unsafe {
            promote_and_fixup(
                &selected,
                &all_roots,
                source.as_ptr().cast(),
                extent,
                std::slice::from_raw_parts_mut(spare.as_mut_ptr().cast(), extent),
                &mut destination,
                &mut descriptors,
                Some(&previous),
            )
        }
        .unwrap();
        let old_value = unsafe { *old_field as usize };
        assert_eq!(untag(old_value), untag(source_root as usize));
        assert_ne!(untag(old_value), source_object as usize);
    }

    #[test]
    fn selected_external_alias_promotes_payload_once_and_fixup_reuses_old_leaf() {
        struct Payload(std::cell::UnsafeCell<*mut u8>);
        // SAFETY: the test owner remains live and is accessed only by the
        // single-threaded collector during this copy.
        unsafe impl crate::external_storage::ExternalPayloadOwner for Payload {
            fn slots(
                &self,
                published: *mut u8,
                kind: ExternalStorageKind,
            ) -> Result<ExternalPointerSlots, ExternalStorageValidationError> {
                if published != self.0.get().cast() {
                    return Err(ExternalStorageValidationError::Untracked(
                        published as usize,
                    ));
                }
                if kind != ExternalStorageKind::BoxedArray {
                    return Err(ExternalStorageValidationError::KindMismatch {
                        expected: kind,
                        actual: ExternalStorageKind::BoxedArray,
                    });
                }
                // SAFETY: this span is the owner-authenticated one-slot view.
                Ok(unsafe { ExternalPointerSlots::from_validated(self.0.get(), 1) })
            }
        }

        let target = TargetDescriptor {
            architecture: Architecture::X86_64,
            endianness: Endianness::Little,
            pointer_width: 64,
            word_width: 64,
            abi: "system-v".into(),
            features: Vec::new(),
        };
        let external =
            Arc::new(ObjectDescriptor::external(ExternalStorageKind::BoxedArray, &target).unwrap());
        let leaf = Arc::new(
            ObjectDescriptor::constructor(1, StorageLayout::for_reps(&target, &[]).unwrap(), None)
                .unwrap(),
        );
        let extent = external.allocation_extent() as usize;
        assert_eq!(extent, leaf.allocation_extent() as usize);
        let source_bytes = extent * 3;
        let mut source = vec![0_u64; source_bytes / 8];
        let mut spare = vec![0_u64; source_bytes / 8];
        let mut descriptors =
            DescriptorSpace::new([Arc::clone(&external), Arc::clone(&leaf)]).unwrap();
        let mut destination =
            DescriptorArena::reserve(source_bytes, [Arc::clone(&external), Arc::clone(&leaf)])
                .unwrap();
        let (selected_object, sibling_object, leaf_object, payload) = unsafe {
            let base = source.as_mut_ptr().cast::<u8>();
            let leaf_object = write_node(base, extent * 2, &leaf, std::ptr::null_mut());
            let payload = Rc::new(Payload(std::cell::UnsafeCell::new(
                (leaf_object as usize | usize::from(leaf.tag())) as *mut u8,
            )));
            let published = payload.0.get().cast::<u8>();
            let selected = write_external(base, 0, &external, published);
            let sibling = write_external(base, extent, &external, published);
            (selected, sibling, leaf_object, payload)
        };
        let mut selected_root = selected_object;
        let mut sibling_root = sibling_object;
        let all_roots = [
            &mut selected_root as *mut *mut u8,
            &mut sibling_root as *mut *mut u8,
        ];
        let selected = [all_roots[0]];
        let result = unsafe {
            promote_and_fixup_with_external(
                &selected,
                &all_roots,
                source.as_ptr().cast(),
                source_bytes,
                std::slice::from_raw_parts_mut(spare.as_mut_ptr().cast(), source_bytes),
                &mut destination,
                &mut descriptors,
                None,
                &*payload,
            )
        }
        .unwrap();
        assert_eq!(result.promoted_bytes, extent * 2);
        assert_eq!(result.nursery_bytes, extent);
        assert_eq!(result.promoted_external_payloads.len(), 1);
        assert_eq!(
            result.promoted_external_payloads[0],
            (payload.0.get() as usize, ExternalStorageKind::BoxedArray)
        );
        let payload_value = unsafe { *payload.0.get() as usize };
        assert_eq!(
            untag(payload_value),
            destination.destination().as_ptr() as usize + extent
        );
        assert_eq!(tag_of(payload_value), leaf.tag());
        let _ = leaf_object;
    }
}
