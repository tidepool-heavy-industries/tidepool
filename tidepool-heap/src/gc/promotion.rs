//! Selective promotion and mandatory sibling fixup form one no-mutator interval.
//!
//! The raw collector owns both the exact source walk and Cheney graph copier.
//! Preparation rejects preexisting Forwarded headers and reserves both copies'
//! scratch before mutation. Copying reuses that authenticated source map; only
//! this operation may introduce forwarding between promotion and sibling fixup.

use super::raw::{self, DescriptorSpace};
use crate::descriptor_region::{DescriptorArena, DescriptorOldSpace};
use crate::execution_descriptor::DescriptorTraceError;

#[derive(Debug)]
pub struct PromotionResult {
    pub promoted_bytes: usize,
    pub nursery_bytes: usize,
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

    let promoted = raw::copy_prevalidated_descriptor_graph(
        selected,
        from_start,
        from_used,
        destination.destination(),
        descriptors,
        previous,
    )
    .map_err(Incomplete)?;
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
    let nursery = raw::copy_prevalidated_descriptor_graph(
        all_roots,
        from_start,
        from_used,
        nursery_spare,
        descriptors,
        Some(&admitted),
    )
    .map_err(Incomplete)?;
    Ok(PromotionResult {
        promoted_bytes: promoted.bytes_copied,
        nursery_bytes: nursery.bytes_copied,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution_descriptor::{DescriptorState, ObjectDescriptor};
    use crate::managed_reference::untag;
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
        std::ptr::write(
            object.add(descriptor.trace_offsets()[0] as usize).cast(),
            child,
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
}
