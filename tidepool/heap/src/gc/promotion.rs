//! Selective promotion and mandatory sibling fixup form one no-mutator interval.
//!
//! The raw collector owns both the exact source walk and Cheney graph copier.
//! Preparation rejects preexisting Forwarded headers and reserves both copies'
//! scratch before mutation. Copying reuses that authenticated source map; only
//! this operation may introduce forwarding between promotion and sibling fixup.
//!
//! Arena compaction ([`compact_descriptor_arenas`]) is the same copy with its
//! SOURCE generalized from one nursery range to a set of retiring arenas. It
//! shares the Cheney loop, the `Forwarded` header protocol and this module's
//! failure discipline: every fallible step precedes the first mutation, and a
//! failure after copying began retires the invocation while retaining both
//! spaces.

use super::raw::{self, CopyFailureOrigin, DescriptorSpace};
use crate::descriptor_region::{DescriptorArena, DescriptorOldSpace, DescriptorSourceSpace};
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PromotionPhase {
    SelectedGraph,
    SiblingFixup,
    ArenaCompaction,
    /// The graph copy finished, but publication bookkeeping failed.
    PostCopy,
}

/// One bounded failure capsule. It keeps the original cause and records only
/// addresses already read by the copier, never a heap view across failure.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{cause} (phase={phase:?}, origin={origin:#x?})")]
pub struct PromotionDiagnostic {
    pub cause: DescriptorTraceError,
    pub phase: PromotionPhase,
    pub origin: Option<CopyFailureOrigin>,
}

impl PromotionDiagnostic {
    pub fn post_copy(cause: DescriptorTraceError) -> Self {
        Self {
            cause,
            phase: PromotionPhase::PostCopy,
            origin: None,
        }
    }

    fn copying(
        phase: PromotionPhase,
        cause: DescriptorTraceError,
        space: &DescriptorSpace,
    ) -> Self {
        Self {
            cause,
            phase,
            origin: space.copy_failure_origin(),
        }
    }
}

/// Preparation has not changed objects. Incomplete means roots or either heap
/// may already have changed: retain all owners and permanently retire the
/// invocation, even if the underlying cause is ordinarily a resource failure.
#[derive(Debug, thiserror::Error)]
pub enum PromotionFailure {
    #[error("promotion preparation: {0}")]
    Preparation(DescriptorTraceError),
    #[error("incomplete promotion; invocation must retire: {0}")]
    Incomplete(PromotionDiagnostic),
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
    .map_err(|cause| {
        Incomplete(PromotionDiagnostic::copying(
            PromotionPhase::SelectedGraph,
            cause,
            descriptors,
        ))
    })?;
    let mut promoted_external_payloads = Vec::new();
    let payload_count = descriptors.visited_external_payloads().count();
    promoted_external_payloads
        .try_reserve(payload_count)
        .map_err(|_| {
            Incomplete(PromotionDiagnostic::post_copy(
                DescriptorTraceError::MetadataAllocation,
            ))
        })?;
    promoted_external_payloads.extend(descriptors.visited_external_payloads());
    destination
        .seal(promoted.bytes_copied)
        .map_err(|cause| Incomplete(PromotionDiagnostic::post_copy(cause)))?;
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
    .map_err(|cause| {
        Incomplete(PromotionDiagnostic::copying(
            PromotionPhase::SiblingFixup,
            cause,
            descriptors,
        ))
    })?;
    Ok(PromotionResult {
        promoted_bytes: promoted.bytes_copied,
        nursery_bytes: nursery.bytes_copied,
        promoted_external_payloads,
    })
}

#[derive(Debug)]
pub struct CompactionResult {
    /// Bytes of live descriptor objects now sealed in the destination arena.
    pub bytes_copied: usize,
    /// Payloads authenticated while copying the live graph.
    pub compacted_external_payloads: Vec<(usize, ExternalStorageKind)>,
}

/// Evacuate every object reachable from `roots` out of `source` into one
/// fresh arena, leaving `Forwarded` headers behind and rewriting every root
/// slot in place.
///
/// `roots` must name every slot outside `source` that can reference a source
/// object: the machine's complete root snapshot, each installed program's
/// root-block words, and each nursery object's reference and external-payload
/// slots. Slots INSIDE `source` are not roots -- they travel with the object
/// that owns them and are rewritten by the Cheney scan of the copy.
/// `admitted` is what stays put (the nursery); static regions are admitted by
/// `descriptors` itself.
///
/// # Safety
/// Source, destination and root slots are disjoint, initialized and owned
/// through success, error and native unwind. No generated frame is live and
/// no mutator runs. `external` must satisfy [`ExternalPayloadOwner`]'s
/// authentication and exclusivity contract. On success the caller retires the
/// source allocations without a fallible step; on `Incomplete` it must not
/// execute, observe or discard either space.
pub unsafe fn compact_descriptor_arenas(
    roots: &[*mut *mut u8],
    source: &dyn DescriptorSourceSpace,
    external_handles: usize,
    destination: &mut DescriptorArena,
    descriptors: &mut DescriptorSpace,
    admitted: Option<&dyn DescriptorOldSpace>,
    external: &dyn ExternalPayloadOwner,
) -> Result<CompactionResult, PromotionFailure> {
    use PromotionFailure::{Incomplete, Preparation};
    raw::prepare_descriptor_copy_from_space(
        roots,
        source,
        external_handles,
        destination.destination(),
        descriptors,
    )
    .map_err(Preparation)?;
    let copied = raw::copy_prevalidated_descriptor_graph_from_space(
        roots,
        source,
        destination.destination(),
        descriptors,
        admitted,
        Some(external),
    )
    .map_err(|cause| {
        Incomplete(PromotionDiagnostic::copying(
            PromotionPhase::ArenaCompaction,
            cause,
            descriptors,
        ))
    })?;
    let mut compacted_external_payloads = Vec::new();
    let payload_count = descriptors.visited_external_payloads().count();
    compacted_external_payloads
        .try_reserve(payload_count)
        .map_err(|_| {
            Incomplete(PromotionDiagnostic::post_copy(
                DescriptorTraceError::MetadataAllocation,
            ))
        })?;
    compacted_external_payloads.extend(descriptors.visited_external_payloads());
    destination
        .seal(copied.bytes_copied)
        .map_err(|cause| Incomplete(PromotionDiagnostic::post_copy(cause)))?;
    Ok(CompactionResult {
        bytes_copied: copied.bytes_copied,
        compacted_external_payloads,
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
        let PromotionFailure::Incomplete(diagnostic) = failure else {
            panic!("a failed copy must retain its owners");
        };
        assert_eq!(diagnostic.phase, PromotionPhase::SelectedGraph);
        assert_eq!(
            diagnostic.cause,
            DescriptorTraceError::InvalidManagedPointer {
                address: root_object as usize + 8
            }
        );
        assert!(matches!(diagnostic.origin,
            Some(CopyFailureOrigin { edge: raw::CopyEdge::ObjectField { descriptor: header, offset, value, .. }, indirection: None })
                if header == descriptor.initial_header_word()
                    && offset == descriptor.trace_offsets()[0] as usize
                    && value == root_object as usize + 8));
        let state = unsafe { descriptor.state(root_object, extent) }.unwrap();
        assert_eq!(state, DescriptorState::Forwarded);
        assert!(!destination.destination().is_empty());
    }

    #[test]
    fn sibling_failure_names_the_root_and_resets_copy_provenance() {
        let descriptor = descriptor();
        let extent = descriptor.allocation_extent() as usize;
        let mut descriptors = DescriptorSpace::new([Arc::clone(&descriptor)]).unwrap();
        for corrupt_sibling in [true, false] {
            let mut source = vec![0_u64; extent / 8];
            let mut spare = vec![0_u64; extent / 8];
            let mut destination =
                DescriptorArena::reserve(extent, [Arc::clone(&descriptor)]).unwrap();
            let mut selected = unsafe {
                write_node(
                    source.as_mut_ptr().cast(),
                    0,
                    &descriptor,
                    std::ptr::null_mut(),
                )
            };
            let invalid = 0xdead_0008;
            let mut sibling = if corrupt_sibling {
                invalid as *mut u8
            } else {
                selected
            };
            let roots = [&mut selected as *mut *mut u8, &mut sibling as *mut *mut u8];
            let result = unsafe {
                promote_and_fixup(
                    &roots[..1],
                    &roots,
                    source.as_ptr().cast(),
                    extent,
                    std::slice::from_raw_parts_mut(spare.as_mut_ptr().cast(), extent),
                    &mut destination,
                    &mut descriptors,
                    None,
                )
            };
            if corrupt_sibling {
                let PromotionFailure::Incomplete(diagnostic) = result.unwrap_err() else {
                    panic!("sibling copy has already followed promotion");
                };
                assert_eq!(diagnostic.phase, PromotionPhase::SiblingFixup);
                assert_eq!(
                    diagnostic.cause,
                    DescriptorTraceError::InvalidManagedPointer { address: invalid }
                );
                assert_eq!(
                    diagnostic.origin,
                    Some(CopyFailureOrigin {
                        edge: raw::CopyEdge::Root {
                            slot: roots[1] as usize,
                            value: invalid
                        },
                        indirection: None,
                    })
                );
                assert_eq!(destination.bytes_used(), extent);
            } else {
                result.unwrap();
                assert_eq!(descriptors.copy_failure_origin(), None);
            }
        }
    }

    #[test]
    fn updated_target_failure_names_only_the_admitted_thunk_hop() {
        let field = descriptor();
        let thunk = Arc::new(
            ObjectDescriptor::new(
                crate::execution_descriptor::ObjectKind::Thunk,
                field.payload().clone(),
                None,
            )
            .unwrap(),
        );
        let extent = thunk.allocation_extent() as usize;
        let mut source = vec![0_u64; extent / 8];
        let mut spare = vec![0_u64; extent / 8];
        let mut destination = DescriptorArena::reserve(extent, [Arc::clone(&thunk)]).unwrap();
        let mut descriptors = DescriptorSpace::new([Arc::clone(&thunk)]).unwrap();
        let invalid = 0xdead_0008;
        let mut root = source.as_mut_ptr().cast::<u8>();
        unsafe {
            thunk.initialize_header(root);
            root.cast::<usize>()
                .write(thunk.initial_header_word() | DescriptorState::Updated as usize);
            root.add(8).cast::<usize>().write(invalid);
        }
        let roots = [&mut root as *mut *mut u8];
        let failure = unsafe {
            promote_and_fixup(
                &roots,
                &roots,
                source.as_ptr().cast(),
                extent,
                std::slice::from_raw_parts_mut(spare.as_mut_ptr().cast(), extent),
                &mut destination,
                &mut descriptors,
                None,
            )
        }
        .unwrap_err();
        let PromotionFailure::Incomplete(diagnostic) = failure else {
            panic!("copy failed")
        };
        assert_eq!(diagnostic.phase, PromotionPhase::SelectedGraph);
        assert_eq!(
            diagnostic.cause,
            DescriptorTraceError::InvalidManagedPointer { address: invalid }
        );
        assert_eq!(
            diagnostic.origin,
            Some(CopyFailureOrigin {
                edge: raw::CopyEdge::Root {
                    slot: roots[0] as usize,
                    value: root as usize
                },
                indirection: Some(raw::CopyIndirection {
                    object: root as usize,
                    descriptor: thunk.initial_header_word(),
                    state: DescriptorState::Updated,
                    target: invalid,
                }),
            })
        );
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

    struct Arenas<'a>(&'a [DescriptorArena]);

    // SAFETY: the test arenas stay borrowed and unmoved for the whole copy.
    unsafe impl DescriptorSourceSpace for Arenas<'_> {
        fn locate_start(&self, address: usize) -> Result<Option<usize>, DescriptorTraceError> {
            for arena in self.0 {
                if let Some(available) = arena.locate_start(address)? {
                    return Ok(Some(available));
                }
            }
            Ok(None)
        }
        fn covers_slot(&self, address: usize) -> bool {
            self.0.iter().any(|arena| arena.covers_slot(address))
        }
        fn overlaps_range(&self, start: usize, end: usize) -> bool {
            self.0.iter().any(|arena| arena.overlaps_range(start, end))
        }
        fn source_bytes(&self) -> usize {
            self.0.iter().map(DescriptorArena::bytes_used).sum()
        }
    }

    /// Two arenas, a cross-arena edge shared by two roots, one dead object:
    /// compaction copies the live pair once, forwards both roots to it, and
    /// leaves the dead object behind.
    #[test]
    fn arena_compaction_copies_a_shared_cross_arena_graph_once() {
        let descriptor = descriptor();
        let extent = descriptor.allocation_extent() as usize;
        let mut first = DescriptorArena::reserve(extent * 2, [Arc::clone(&descriptor)]).unwrap();
        let mut second = DescriptorArena::reserve(extent, [Arc::clone(&descriptor)]).unwrap();
        let tag = usize::from(descriptor.tag());
        let child = unsafe {
            let child = write_node(
                second.destination().as_mut_ptr(),
                0,
                &descriptor,
                std::ptr::null_mut(),
            );
            let base = first.destination().as_mut_ptr();
            write_node(base, 0, &descriptor, (child as usize | tag) as *mut u8);
            write_node(base, extent, &descriptor, std::ptr::null_mut());
            child
        };
        first.seal(extent * 2).unwrap();
        second.seal(extent).unwrap();
        let parent = first.destination().as_mut_ptr();
        let arenas = [first, second];
        let mut parent_root = (parent as usize | tag) as *mut u8;
        let mut child_root = (child as usize | tag) as *mut u8;
        let roots = [
            &mut parent_root as *mut *mut u8,
            &mut child_root as *mut *mut u8,
        ];
        let mut descriptors = DescriptorSpace::new([Arc::clone(&descriptor)]).unwrap();
        let mut destination =
            DescriptorArena::reserve(extent * 3, [Arc::clone(&descriptor)]).unwrap();
        let result = unsafe {
            compact_descriptor_arenas(
                &roots,
                &Arenas(&arenas),
                0,
                &mut destination,
                &mut descriptors,
                None,
                &NoPayloads,
            )
        }
        .unwrap();
        assert_eq!(result.bytes_copied, extent * 2);
        assert_eq!(destination.bytes_used(), extent * 2);
        let range = destination.allocation_range();
        assert!(range.contains(&untag(parent_root as usize)));
        let field = unsafe {
            std::ptr::read(
                (untag(parent_root as usize) as *const u8)
                    .add(descriptor.trace_offsets()[0] as usize)
                    .cast::<usize>(),
            )
        };
        assert_eq!(
            field, child_root as usize,
            "the shared child is copied once"
        );
        assert_eq!(tag_of(field), descriptor.tag());
        assert_eq!(
            unsafe { descriptor.state(child, extent) }.unwrap(),
            DescriptorState::Forwarded
        );
        assert_eq!(
            unsafe { descriptor.state(parent.add(extent), extent) }.unwrap(),
            DescriptorState::Live,
            "the dead object is never visited"
        );

        let invalid = 0xdead_0008;
        let mut broken = DescriptorArena::reserve(extent, [Arc::clone(&descriptor)]).unwrap();
        let mut root = unsafe {
            write_node(
                broken.destination().as_mut_ptr(),
                0,
                &descriptor,
                invalid as *mut u8,
            )
        };
        broken.seal(extent).unwrap();
        let source = [broken];
        let roots = [&mut root as *mut *mut u8];
        let mut destination = DescriptorArena::reserve(extent, [Arc::clone(&descriptor)]).unwrap();
        let failure = unsafe {
            compact_descriptor_arenas(
                &roots,
                &Arenas(&source),
                0,
                &mut destination,
                &mut descriptors,
                None,
                &NoPayloads,
            )
        }
        .unwrap_err();
        let PromotionFailure::Incomplete(diagnostic) = failure else {
            panic!("arena copy failed")
        };
        assert_eq!(diagnostic.phase, PromotionPhase::ArenaCompaction);
        assert_eq!(
            diagnostic.cause,
            DescriptorTraceError::InvalidManagedPointer { address: invalid }
        );
        assert!(matches!(diagnostic.origin,
            Some(CopyFailureOrigin { edge: raw::CopyEdge::ObjectField { descriptor: header, offset, value, .. }, indirection: None })
                if header == descriptor.initial_header_word() && offset == descriptor.trace_offsets()[0] as usize && value == invalid));
    }

    struct NoPayloads;

    // SAFETY: owns no payloads; every lookup is refused.
    unsafe impl ExternalPayloadOwner for NoPayloads {
        fn slots(
            &self,
            published: *mut u8,
            _kind: ExternalStorageKind,
        ) -> Result<ExternalPointerSlots, ExternalStorageValidationError> {
            Err(ExternalStorageValidationError::Untracked(
                published as usize,
            ))
        }
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

        // A later copy authenticates the same external owner but refuses its
        // corrupt managed element without reading the element's target.
        let invalid = 0xdead_0008;
        unsafe { *payload.0.get() = invalid as *mut u8 };
        let mut next_source = vec![0_u64; extent / 8];
        let mut next_spare = vec![0_u64; extent / 8];
        let mut next_destination =
            DescriptorArena::reserve(extent, [Arc::clone(&external)]).unwrap();
        let mut next_root = unsafe {
            write_external(
                next_source.as_mut_ptr().cast(),
                0,
                &external,
                payload.0.get().cast(),
            )
        };
        let roots = [&mut next_root as *mut *mut u8];
        let failure = unsafe {
            promote_and_fixup_with_external(
                &roots,
                &roots,
                next_source.as_ptr().cast(),
                extent,
                std::slice::from_raw_parts_mut(next_spare.as_mut_ptr().cast(), extent),
                &mut next_destination,
                &mut descriptors,
                None,
                &*payload,
            )
        }
        .unwrap_err();
        let PromotionFailure::Incomplete(diagnostic) = failure else {
            panic!("external copy failed")
        };
        assert_eq!(diagnostic.phase, PromotionPhase::SelectedGraph);
        assert_eq!(
            diagnostic.cause,
            DescriptorTraceError::InvalidManagedPointer { address: invalid }
        );
        assert!(matches!(diagnostic.origin,
            Some(CopyFailureOrigin { edge: raw::CopyEdge::ExternalSlot { descriptor: header, payload: published, kind: ExternalStorageKind::BoxedArray, slot, value, .. }, indirection: None })
                if header == external.initial_header_word() && published == payload.0.get() as usize
                    && slot == payload.0.get() as usize && value == invalid));
    }
}
