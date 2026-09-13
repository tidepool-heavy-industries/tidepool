//! Non-forcing, bounded materialization while invocation storage remains owned.

use std::collections::BTreeMap;
use std::sync::Arc;

use tidepool_bridge::Value;
use tidepool_heap::execution_descriptor::{
    DescriptorState, DescriptorTraceError, ObjectDescriptor, ObjectKind,
};
use tidepool_heap::managed_reference::{tag_of, tag_valid, untag};
use tidepool_heap::static_region::StaticRegion;
use tidepool_repr::execution_schema::{RuntimeRep, StorageLayout};
use tidepool_repr::Literal;

use super::{ConstructorObservation, DescriptorMeaning, DescriptorMetadata};

#[derive(Debug, thiserror::Error)]
pub enum ObservationFailure {
    #[error("observation budget {limit} exhausted")]
    BudgetExceeded { limit: usize },
    #[error("observation snapshot allocation failed")]
    AllocationFailed,
    #[error("cannot observe {0:?} without forcing or applying it")]
    Unobservable(ObjectKind),
    #[error("representation {0:?} is not a materialized host value")]
    Representation(RuntimeRep),
    #[error(transparent)]
    Integrity(#[from] DescriptorTraceError),
}

#[derive(Clone, Copy)]
pub(super) struct ObservationSeed {
    pub(super) word: usize,
    pub(super) rep: RuntimeRep,
}

pub(super) struct ObservationBudget {
    pub(super) remaining: usize,
    pub(super) limit: usize,
}

impl ObservationBudget {
    /// Materialization costs one unit per value node and per copied payload
    /// byte. An atomic byte-array leaf cannot bypass the observation bound.
    pub(super) fn charge_bytes(&mut self, bytes: usize) -> Result<(), ObservationFailure> {
        self.remaining = self.remaining.checked_sub(bytes)
            .ok_or(ObservationFailure::BudgetExceeded { limit: self.limit })?;
        Ok(())
    }
}

pub(super) enum ObservationFrame<X> {
    Leaf(Value),
    Constructor(tidepool_repr::DataConId, Vec<X>),
}

impl recursion::MappableFrame for ObservationFrame<recursion::PartiallyApplied> {
    type Frame<X> = ObservationFrame<X>;

    fn map_frame<A, B>(input: Self::Frame<A>, mut f: impl FnMut(A) -> B) -> Self::Frame<B> {
        match input {
            ObservationFrame::Leaf(value) => ObservationFrame::Leaf(value),
            ObservationFrame::Constructor(identity, fields) => {
                ObservationFrame::Constructor(identity, fields.into_iter().map(&mut f).collect())
            }
        }
    }
}

pub(super) struct ObservationHeap<'a> {
    nursery: &'a [u64],
    statics: &'a StaticRegion,
    old_space: Option<&'a dyn tidepool_heap::descriptor_region::DescriptorOldSpace>,
    descriptors: BTreeMap<usize, Arc<ObjectDescriptor>>,
    starts: Vec<u64>,
    constructors: Option<&'a BTreeMap<usize, ConstructorObservation>>,
    registry: Option<&'a BTreeMap<usize, DescriptorMetadata>>,
}

/// Extend exact-start metadata across newly initialized nursery words. The
/// caller resets `starts` and `scanned_words` after a GC generation changes.
pub(super) fn append_exact_starts(
    nursery: &[u64],
    registry: &BTreeMap<usize, DescriptorMetadata>,
    starts: &mut Vec<u64>,
    scanned_words: &mut usize,
) -> Result<(), ObservationFailure> {
    let descriptors: BTreeMap<_, _> = registry
        .values()
        .map(|metadata| {
            (
                metadata.descriptor.initial_header_word(),
                &metadata.descriptor,
            )
        })
        .collect();
    starts.resize(nursery.len().div_ceil(64), 0);
    let mut offset = *scanned_words;
    while offset < nursery.len() {
        let header = nursery[offset] as usize;
        let descriptor =
            descriptors
                .get(&(header & !7))
                .ok_or(DescriptorTraceError::UnknownDescriptor {
                    address: header & !7,
                })?;
        let available = (nursery.len() - offset) * 8;
        let state = unsafe { descriptor.state(nursery.as_ptr().add(offset).cast(), available)? };
        if !matches!(
            state,
            DescriptorState::Live | DescriptorState::Evaluating | DescriptorState::Updated
        ) {
            return Err(DescriptorTraceError::StateForKind {
                state,
                kind: descriptor.kind(),
            }
            .into());
        }
        let extent = descriptor.allocation_extent() as usize;
        if extent < 16 || extent % 8 != 0 {
            return Err(DescriptorTraceError::InvalidRange.into());
        }
        starts[offset / 64] |= 1_u64 << (offset % 64);
        offset = offset
            .checked_add(extent / 8)
            .ok_or(DescriptorTraceError::InvalidRange)?;
    }
    *scanned_words = nursery.len();
    Ok(())
}

impl<'a> ObservationHeap<'a> {
    pub fn new(
        nursery: &'a [u64],
        statics: &'a StaticRegion,
        descriptors: impl IntoIterator<Item = Arc<ObjectDescriptor>>,
        constructors: &'a BTreeMap<usize, ConstructorObservation>,
    ) -> Result<Self, ObservationFailure> {
        Self::build(nursery, statics, descriptors, Some(constructors), None)
    }

    pub fn new_with_registry(
        nursery: &'a [u64],
        statics: &'a StaticRegion,
        registry: &'a BTreeMap<usize, DescriptorMetadata>,
    ) -> Result<Self, ObservationFailure> {
        Self::build(
            nursery,
            statics,
            registry
                .values()
                .map(|metadata| Arc::clone(&metadata.descriptor)),
            None,
            Some(registry),
        )
    }

    pub(super) fn new_with_registry_and_starts(
        nursery: &'a [u64],
        statics: &'a StaticRegion,
        registry: &'a BTreeMap<usize, DescriptorMetadata>,
        starts: &[u64],
        old_space: Option<&'a dyn tidepool_heap::descriptor_region::DescriptorOldSpace>,
    ) -> Result<Self, ObservationFailure> {
        let descriptors: BTreeMap<_, _> = registry
            .values()
            .map(|metadata| {
                (
                    metadata.descriptor.initial_header_word(),
                    Arc::clone(&metadata.descriptor),
                )
            })
            .collect();
        Ok(Self {
            nursery,
            statics,
            old_space,
            descriptors,
            starts: starts.to_vec(),
            constructors: None,
            registry: Some(registry),
        })
    }

    fn build(
        nursery: &'a [u64],
        statics: &'a StaticRegion,
        descriptors: impl IntoIterator<Item = Arc<ObjectDescriptor>>,
        constructors: Option<&'a BTreeMap<usize, ConstructorObservation>>,
        registry: Option<&'a BTreeMap<usize, DescriptorMetadata>>,
    ) -> Result<Self, ObservationFailure> {
        let descriptors: BTreeMap<_, _> = descriptors
            .into_iter()
            .map(|descriptor| (descriptor.initial_header_word(), descriptor))
            .collect();
        let mut starts = Vec::new();
        starts
            .try_reserve_exact(nursery.len().div_ceil(64))
            .map_err(|_| DescriptorTraceError::MetadataAllocation)?;
        starts.resize(nursery.len().div_ceil(64), 0_u64);
        let mut offset = 0;
        while offset < nursery.len() {
            let header = nursery[offset] as usize;
            let descriptor =
                descriptors
                    .get(&(header & !7))
                    .ok_or(DescriptorTraceError::UnknownDescriptor {
                        address: header & !7,
                    })?;
            let available = (nursery.len() - offset) * 8;
            // The borrowed slice proves allocation bounds before any object read.
            let state =
                unsafe { descriptor.state(nursery.as_ptr().add(offset).cast(), available)? };
            if !matches!(
                state,
                DescriptorState::Live | DescriptorState::Evaluating | DescriptorState::Updated
            ) {
                return Err(DescriptorTraceError::StateForKind {
                    state,
                    kind: descriptor.kind(),
                }
                .into());
            }
            let extent = descriptor.allocation_extent() as usize;
            if extent < 16 || extent % 8 != 0 {
                return Err(DescriptorTraceError::InvalidRange.into());
            }
            starts[offset / 64] |= 1_u64 << (offset % 64);
            offset += extent / 8;
        }
        Ok(Self {
            nursery,
            statics,
            old_space: None,
            descriptors,
            starts,
            constructors,
            registry,
        })
    }

    fn object(
        &self,
        encoded: usize,
    ) -> Result<(&ObjectDescriptor, *const u8, DescriptorState), ObservationFailure> {
        let address = untag(encoded);
        let static_pointer = self.statics.admit(encoded)?.is_some();
        let old_pointer = if static_pointer {
            None
        } else if let Some(owner) = self.old_space {
            owner.admit(encoded)?
        } else {
            None
        };
        let available = if static_pointer {
            self.statics
                .address_range()
                .end
                .checked_sub(address)
                .ok_or(DescriptorTraceError::InvalidManagedPointer { address })?
        } else if old_pointer.is_some() {
            // `admit` proved an exact initialized start and the arena keeps
            // its bytes stable for this borrow; read the header only now.
            let header = unsafe { std::ptr::read(address as *const usize) };
            let descriptor = self.descriptors.get(&(header & !7)).ok_or(
                DescriptorTraceError::UnknownDescriptor {
                    address: header & !7,
                },
            )?;
            descriptor.allocation_extent() as usize
        } else {
            let base = self.nursery.as_ptr() as usize;
            let offset = address
                .checked_sub(base)
                .ok_or(DescriptorTraceError::InvalidManagedPointer { address })?;
            if offset % 8 != 0
                || offset / 8 >= self.nursery.len()
                || self.starts[offset / 8 / 64] & (1_u64 << (offset / 8 % 64)) == 0
            {
                return Err(DescriptorTraceError::InvalidManagedPointer { address }.into());
            }
            self.nursery
                .len()
                .checked_mul(std::mem::size_of::<u64>())
                .and_then(|bytes| bytes.checked_sub(offset))
                .ok_or(DescriptorTraceError::InvalidManagedPointer { address })?
        };
        let header = unsafe { std::ptr::read(address as *const usize) };
        let descriptor = self.descriptors.get(&(header & !7)).ok_or(
            DescriptorTraceError::UnknownDescriptor {
                address: header & !7,
            },
        )?;
        // Exact-start membership was proved by a validated immutable region,
        // retained arena, or nursery walk; all owners remain borrowed through
        // this observation.
        let state = unsafe { descriptor.state(address as *const u8, available)? };
        if !tag_valid(
            tag_of(encoded),
            descriptor.kind(),
            state,
            descriptor.constructor_tag(),
        ) {
            return Err(DescriptorTraceError::InvalidManagedTag {
                address,
                tag: tag_of(encoded),
            }
            .into());
        }
        Ok((descriptor, address as *const u8, state))
    }

    pub(super) fn validate_reference(&self, encoded: usize) -> Result<(), ObservationFailure> {
        self.object(encoded).map(|_| ())
    }

    /// Result storage is already registered as roots by the invocation owner.
    /// No forcing, native call, or collection occurs anywhere in this traversal.
    pub fn observe_results(
        &self,
        words: &[u64],
        reps: &[RuntimeRep],
        layout: &StorageLayout,
        budget: usize,
    ) -> Result<Vec<Value>, ObservationFailure> {
        let mut budget = ObservationBudget {
            remaining: budget,
            limit: budget,
        };
        let mut results = Vec::new();
        for seed in snapshot_results(words, reps, layout)? {
            let value = recursion::try_expand_and_collapse::<
                ObservationFrame<recursion::PartiallyApplied>,
                _,
                _,
                _,
            >(
                seed,
                |seed| self.expand(seed, &mut budget),
                |frame| match frame {
                    ObservationFrame::Leaf(value) => Ok(value),
                    ObservationFrame::Constructor(identity, fields) => {
                        let mut fields = fields;
                        fields.reverse();
                        Ok(Value::Con(identity, fields))
                    }
                },
            )?;
            results.push(value);
        }
        Ok(results)
    }

    pub(super) fn expand(
        &self,
        mut seed: ObservationSeed,
        budget: &mut ObservationBudget,
    ) -> Result<ObservationFrame<ObservationSeed>, ObservationFailure> {
        loop {
            if budget.remaining == 0 {
                return Err(ObservationFailure::BudgetExceeded {
                    limit: budget.limit,
                });
            }
            budget.remaining -= 1;

            match seed.rep {
                RuntimeRep::Void => unreachable!("void results are omitted before observation"),
                RuntimeRep::Address => return Err(ObservationFailure::Representation(seed.rep)),
                RuntimeRep::Int(bits) => {
                    return Ok(ObservationFrame::Leaf(Value::Lit(Literal::LitInt(
                        signed_value(seed.word, bits)?,
                    ))))
                }
                RuntimeRep::Word(bits) => {
                    return Ok(ObservationFrame::Leaf(Value::Lit(Literal::LitWord(
                        unsigned_value(seed.word, bits)?,
                    ))))
                }
                RuntimeRep::Float(32) => {
                    return Ok(ObservationFrame::Leaf(Value::Lit(Literal::LitFloat(
                        (seed.word as u32).into(),
                    ))))
                }
                RuntimeRep::Float(64) => {
                    return Ok(ObservationFrame::Leaf(Value::Lit(Literal::LitDouble(
                        seed.word as u64,
                    ))))
                }
                RuntimeRep::Float(bits) => {
                    return Err(ObservationFailure::Representation(RuntimeRep::Float(bits)))
                }
                RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef => {
                    let (descriptor, object, state) = self.object(seed.word)?;
                    if state == DescriptorState::Updated {
                        seed.word = read_object(
                            object,
                            descriptor,
                            tidepool_heap::execution_descriptor::FORWARDING_POINTER_OFFSET,
                            std::mem::size_of::<usize>(),
                        )?;
                        continue;
                    }
                    match descriptor.kind() {
                        ObjectKind::Constructor => {
                            let observation = self
                                .registry
                                .and_then(|registry| {
                                    registry.get(&descriptor.initial_header_word())
                                })
                                .and_then(|metadata| match &metadata.meaning {
                                    DescriptorMeaning::Constructor(observation) => {
                                        Some(observation)
                                    }
                                    DescriptorMeaning::Callable { .. } => None,
                                    DescriptorMeaning::Pap { .. } => None,
                                    DescriptorMeaning::External => None,
                                })
                                .or_else(|| {
                                    self.constructors.and_then(|constructors| {
                                        constructors.get(&descriptor.initial_header_word())
                                    })
                                })
                                .ok_or(ObservationFailure::Integrity(
                                    DescriptorTraceError::InvalidRange,
                                ))?;
                            let logical = descriptor.payload().logical_to_stored();
                            if logical.len() != observation.fields.len() {
                                return Err(ObservationFailure::Integrity(
                                    DescriptorTraceError::InvalidRange,
                                ));
                            }
                            let mut fields = Vec::new();
                            fields.try_reserve(observation.fields.len()).map_err(|_| {
                                ObservationFailure::Integrity(
                                    DescriptorTraceError::MetadataAllocation,
                                )
                            })?;
                            for (index, rep) in observation.fields.iter().copied().enumerate() {
                                let Some(stored_index) = logical[index] else {
                                    if rep == RuntimeRep::Void {
                                        continue;
                                    }
                                    return Err(ObservationFailure::Integrity(
                                        DescriptorTraceError::InvalidRange,
                                    ));
                                };
                                let Some(field) =
                                    descriptor.payload().fields().get(stored_index as usize)
                                else {
                                    return Err(ObservationFailure::Integrity(
                                        DescriptorTraceError::InvalidRange,
                                    ));
                                };
                                if field.rep() != rep {
                                    return Err(ObservationFailure::Integrity(
                                        DescriptorTraceError::InvalidRange,
                                    ));
                                }
                                let offset = descriptor
                                    .payload_base()
                                    .checked_add(field.offset())
                                    .ok_or(ObservationFailure::Integrity(
                                    DescriptorTraceError::InvalidRange,
                                ))? as usize;
                                let word =
                                    read_object(object, descriptor, offset, field.size() as usize)?;
                                fields.push(ObservationSeed { word, rep });
                            }
                            // `recursion` visits frame children through a LIFO worklist.
                            // Keep the logical source order in the final value while
                            // presenting the first child to that worklist first.
                            fields.reverse();
                            return Ok(ObservationFrame::Constructor(observation.identity, fields));
                        }
                        kind => return Err(ObservationFailure::Unobservable(kind)),
                    }
                }
            }
        }
    }
}

pub(super) fn snapshot_results(
    words: &[u64],
    reps: &[RuntimeRep],
    layout: &StorageLayout,
) -> Result<Vec<ObservationSeed>, ObservationFailure> {
    if reps.len() != layout.logical_to_stored().len() {
        return Err(ObservationFailure::Integrity(
            DescriptorTraceError::InvalidRange,
        ));
    }
    let mut seeds = Vec::new();
    seeds
        .try_reserve_exact(reps.len())
        .map_err(|_| ObservationFailure::Integrity(DescriptorTraceError::MetadataAllocation))?;
    for (logical_index, rep) in reps.iter().copied().enumerate() {
        let Some(stored_index) = layout.logical_to_stored()[logical_index] else {
            continue;
        };
        let Some(field) = layout.fields().get(stored_index as usize) else {
            return Err(ObservationFailure::Integrity(
                DescriptorTraceError::InvalidRange,
            ));
        };
        if field.rep() != rep {
            return Err(ObservationFailure::Integrity(
                DescriptorTraceError::InvalidRange,
            ));
        }
        seeds.push(ObservationSeed {
            word: read_words(words, field.offset() as usize, field.size() as usize)?,
            rep,
        });
    }
    Ok(seeds)
}

pub(super) fn read_words(
    words: &[u64],
    offset: usize,
    size: usize,
) -> Result<usize, ObservationFailure> {
    let bytes = words.len().checked_mul(std::mem::size_of::<u64>()).ok_or(
        ObservationFailure::Integrity(DescriptorTraceError::InvalidRange),
    )?;
    let end = offset
        .checked_add(size)
        .ok_or(ObservationFailure::Integrity(
            DescriptorTraceError::InvalidRange,
        ))?;
    if size > std::mem::size_of::<usize>() || end > bytes {
        return Err(ObservationFailure::Integrity(
            DescriptorTraceError::InvalidRange,
        ));
    }
    let mut encoded = [0_u8; std::mem::size_of::<usize>()];
    let source =
        unsafe { std::slice::from_raw_parts(words.as_ptr().cast::<u8>().add(offset), size) };
    encoded[..size].copy_from_slice(source);
    Ok(usize::from_ne_bytes(encoded))
}

fn read_object(
    object: *const u8,
    descriptor: &ObjectDescriptor,
    offset: usize,
    size: usize,
) -> Result<usize, ObservationFailure> {
    let end = offset
        .checked_add(size)
        .ok_or(ObservationFailure::Integrity(
            DescriptorTraceError::InvalidRange,
        ))?;
    if size > std::mem::size_of::<usize>() || end > descriptor.allocation_extent() as usize {
        return Err(ObservationFailure::Integrity(
            DescriptorTraceError::InvalidRange,
        ));
    }
    let mut encoded = [0_u8; std::mem::size_of::<usize>()];
    let source = unsafe { std::slice::from_raw_parts(object.add(offset), size) };
    encoded[..size].copy_from_slice(source);
    Ok(usize::from_ne_bytes(encoded))
}

fn unsigned_value(value: usize, bits: u8) -> Result<u64, ObservationFailure> {
    if !matches!(bits, 8 | 16 | 32 | 64) {
        return Err(ObservationFailure::Representation(RuntimeRep::Word(bits)));
    }
    let mask = if bits == 64 {
        u64::MAX
    } else {
        (1_u64 << bits) - 1
    };
    Ok((value as u64) & mask)
}

fn signed_value(value: usize, bits: u8) -> Result<i64, ObservationFailure> {
    if !matches!(bits, 8 | 16 | 32 | 64) {
        return Err(ObservationFailure::Representation(RuntimeRep::Int(bits)));
    }
    let unsigned = unsigned_value(value, bits)?;
    if bits == 64 {
        return Ok(unsigned as i64);
    }
    let shift = 64 - u32::from(bits);
    Ok(((unsigned << shift) as i64) >> shift)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use tidepool_heap::execution_descriptor::ObjectDescriptor;
    use tidepool_heap::static_region::StaticImage;
    use tidepool_repr::execution_schema::{
        Architecture, Endianness, RuntimeRep, StorageLayout, TargetDescriptor,
    };
    use tidepool_repr::DataConId;

    fn target() -> TargetDescriptor {
        TargetDescriptor {
            architecture: Architecture::X86_64,
            endianness: Endianness::Little,
            pointer_width: 64,
            word_width: 64,
            abi: "sysv64".into(),
            features: vec![],
        }
    }

    fn statics() -> StaticRegion {
        StaticImage::new(
            vec![],
            vec![],
            BTreeMap::new(),
            Vec::<Arc<ObjectDescriptor>>::new(),
        )
        .unwrap()
        .instantiate()
        .unwrap()
    }

    fn words(bytes: &[u8]) -> Vec<u64> {
        let mut words = vec![0_u64; bytes.len().div_ceil(8)];
        for (index, byte) in bytes.iter().copied().enumerate() {
            words[index / 8] |= u64::from(byte) << (index % 8 * 8);
        }
        words
    }

    fn constructor_chain(
        depth: usize,
        cycle: bool,
    ) -> (
        Vec<u64>,
        Vec<Arc<ObjectDescriptor>>,
        BTreeMap<usize, ConstructorObservation>,
        usize,
    ) {
        let parent_layout = StorageLayout::for_reps(&target(), &[RuntimeRep::LiftedRef]).unwrap();
        let leaf_layout = StorageLayout::for_reps(&target(), &[RuntimeRep::Int(8)]).unwrap();
        let parent = Arc::new(ObjectDescriptor::constructor(1, parent_layout, None).unwrap());
        let leaf = Arc::new(ObjectDescriptor::constructor(1, leaf_layout, None).unwrap());
        let mut nursery = vec![0_u64; (depth + 1) * 2];
        for index in 0..depth {
            let object = unsafe { nursery.as_mut_ptr().add(index * 2).cast::<u8>() };
            unsafe { parent.initialize_header(object) };
            let child = if cycle && index + 1 == depth {
                nursery.as_ptr() as usize | usize::from(parent.tag())
            } else {
                nursery.as_ptr().wrapping_add((index + 1) * 2) as usize | usize::from(parent.tag())
            };
            nursery[index * 2 + 1] = child as u64;
        }
        let object = unsafe { nursery.as_mut_ptr().add(depth * 2).cast::<u8>() };
        unsafe {
            leaf.initialize_header(object);
            object.add(leaf.payload_base() as usize).write(42);
        }
        let constructors = BTreeMap::from([
            (
                parent.initial_header_word(),
                ConstructorObservation {
                    identity: DataConId(10),
                    fields: vec![RuntimeRep::LiftedRef],
                },
            ),
            (
                leaf.initial_header_word(),
                ConstructorObservation {
                    identity: DataConId(20),
                    fields: vec![RuntimeRep::Int(8)],
                },
            ),
        ]);
        (nursery, vec![parent, leaf], constructors, depth)
    }

    #[test]
    fn observes_zero_multiple_void_and_sized_scalars_in_source_order() {
        let statics = statics();
        let reps = vec![
            RuntimeRep::Void,
            RuntimeRep::Int(8),
            RuntimeRep::Word(16),
            RuntimeRep::Float(32),
            RuntimeRep::Float(64),
        ];
        let layout = StorageLayout::for_reps(&target(), &reps).unwrap();
        let mut bytes = vec![0_u8; layout.payload_size() as usize];
        bytes[0] = 0xff;
        bytes[2..4].copy_from_slice(&0x1234_u16.to_ne_bytes());
        bytes[4..8].copy_from_slice(&0x7fc0_0001_u32.to_ne_bytes());
        bytes[8..16].copy_from_slice(&0x7ff8_0000_0000_0001_u64.to_ne_bytes());
        let words = words(&bytes);
        let constructors = BTreeMap::new();
        let heap = ObservationHeap::new(
            &[],
            &statics,
            Vec::<Arc<ObjectDescriptor>>::new(),
            &constructors,
        )
        .unwrap();
        let result = heap.observe_results(&words, &reps, &layout, 4).unwrap();
        assert_eq!(result.len(), 4);
        assert!(matches!(
            &result[0],
            Value::Lit(Literal::LitInt(value)) if *value == -1
        ));
        assert!(matches!(
            &result[1],
            Value::Lit(Literal::LitWord(value)) if *value == 0x1234
        ));
        assert!(matches!(
            &result[2],
            Value::Lit(Literal::LitFloat(value)) if *value == 0x7fc0_0001
        ));
        assert!(matches!(
            &result[3],
            Value::Lit(Literal::LitDouble(value)) if *value == 0x7ff8_0000_0000_0001
        ));

        let empty = StorageLayout::for_reps(&target(), &[]).unwrap();
        assert!(heap
            .observe_results(&[], &[], &empty, 0)
            .unwrap()
            .is_empty());
        let limited = heap.observe_results(&words, &reps, &layout, 1);
        assert!(matches!(
            limited,
            Err(ObservationFailure::BudgetExceeded { limit: 1 })
        ));
    }

    #[test]
    fn updated_thunk_chase_masks_the_header_and_materializes_its_target() {
        let statics = statics();
        let thunk_layout = StorageLayout::for_reps(&target(), &[]).unwrap();
        let thunk = Arc::new(ObjectDescriptor::new(ObjectKind::Thunk, thunk_layout, None).unwrap());
        let leaf_layout = StorageLayout::for_reps(&target(), &[]).unwrap();
        let leaf = Arc::new(ObjectDescriptor::constructor(1, leaf_layout, None).unwrap());
        let mut nursery = vec![0_u64; 4];
        nursery[0] = (thunk.initial_header_word() | DescriptorState::Updated as usize) as u64;
        nursery[1] = (unsafe { nursery.as_ptr().add(2) } as usize | usize::from(leaf.tag())) as u64;
        unsafe { leaf.initialize_header(nursery.as_mut_ptr().add(2).cast()) };
        let constructors = BTreeMap::from([(
            leaf.initial_header_word(),
            ConstructorObservation {
                identity: DataConId(40),
                fields: vec![],
            },
        )]);
        let heap = ObservationHeap::new(
            &nursery,
            &statics,
            vec![Arc::clone(&thunk), Arc::clone(&leaf)],
            &constructors,
        )
        .unwrap();
        let root = nursery.as_ptr() as usize;
        let reps = [RuntimeRep::LiftedRef];
        let layout = StorageLayout::for_reps(&target(), &reps).unwrap();
        assert!(matches!(
            heap.observe_results(&[root as u64], &reps, &layout, 2),
            Ok(values) if matches!(values.as_slice(), [Value::Con(DataConId(40), fields)] if fields.is_empty())
        ));
        let (_, _, state) = heap.object(root).unwrap();
        assert_eq!(state, DescriptorState::Updated);
    }

    #[test]
    fn observes_deep_constructors_without_recursion_and_uses_owner_identity() {
        if std::env::var_os("TIDEPOOL_OBSERVE_CHILD").is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "prepared_program::observe::tests::observes_deep_constructors_without_recursion_and_uses_owner_identity",
                    "--nocapture",
                ])
                .env("TIDEPOOL_OBSERVE_CHILD", "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "child observation failed: {output:?}"
            );
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                stdout.contains(
                    "observes_deep_constructors_without_recursion_and_uses_owner_identity ... ok"
                ),
                "child test did not run successfully: {stdout}"
            );
            return;
        }
        std::thread::Builder::new()
            .name("observe-deep".into())
            .stack_size(64 * 1024)
            .spawn(observe_deep_chain)
            .unwrap()
            .join()
            .unwrap();
    }

    fn observe_deep_chain() {
        let statics = statics();
        let (nursery, descriptors, constructors, depth) = constructor_chain(20_000, false);
        let root = nursery.as_ptr() as usize | usize::from(descriptors[0].tag());
        let reps = [RuntimeRep::LiftedRef];
        let layout = StorageLayout::for_reps(&target(), &reps).unwrap();
        let heap = ObservationHeap::new(&nursery, &statics, descriptors, &constructors).unwrap();
        // The chain contains `depth` parent constructors, one leaf constructor,
        // and the leaf's Int(8) payload: every constructor/scalar expansion
        // consumes one budget unit.
        let budget = depth + 2;
        let result = heap
            .observe_results(&[root as u64], &reps, &layout, budget)
            .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].node_count(), depth + 2);
        let Value::Con(identity, fields) = &result[0] else {
            panic!("expected constructor result")
        };
        assert_eq!(*identity, DataConId(10));
        let mut leaf = &result[0];
        for _ in 0..depth {
            let Value::Con(_, children) = leaf else {
                panic!("expected constructor chain")
            };
            leaf = &children[0];
        }
        let Value::Con(leaf_identity, leaf_fields) = leaf else {
            panic!("expected leaf constructor")
        };
        assert_eq!(*leaf_identity, DataConId(20));
        assert_eq!(leaf_fields.len(), 1);
        assert!(matches!(
            &leaf_fields[0],
            Value::Lit(Literal::LitInt(value)) if *value == 42
        ));
        assert_eq!(fields.len(), 1);
    }

    #[test]
    fn cyclic_constructors_exhaust_budget_and_reject_unobservable_shapes() {
        let statics = statics();
        let (nursery, descriptors, constructors, _) = constructor_chain(1, true);
        let root = nursery.as_ptr() as usize | usize::from(descriptors[0].tag());
        let reps = [RuntimeRep::LiftedRef];
        let layout = StorageLayout::for_reps(&target(), &reps).unwrap();
        let heap = ObservationHeap::new(&nursery, &statics, descriptors, &constructors).unwrap();
        assert!(matches!(
            heap.observe_results(&[root as u64], &reps, &layout, 3),
            Err(ObservationFailure::BudgetExceeded { limit: 3 })
        ));

        let function_layout = StorageLayout::for_reps(&target(), &[]).unwrap();
        let function =
            Arc::new(ObjectDescriptor::new(ObjectKind::Function, function_layout, None).unwrap());
        let mut function_nursery = vec![0_u64; 2];
        unsafe { function.initialize_header(function_nursery.as_mut_ptr().cast()) };
        let function_root = function_nursery.as_ptr() as usize | usize::from(function.tag());
        let function_constructors = BTreeMap::new();
        let function_heap = ObservationHeap::new(
            &function_nursery,
            &statics,
            vec![function],
            &function_constructors,
        )
        .unwrap();
        assert!(matches!(
            function_heap.observe_results(&[function_root as u64], &reps, &layout, 1),
            Err(ObservationFailure::Unobservable(ObjectKind::Function))
        ));

        let pap_layout = StorageLayout::for_reps(&target(), &[]).unwrap();
        let pap = Arc::new(ObjectDescriptor::new(ObjectKind::Pap, pap_layout, None).unwrap());
        let mut pap_nursery = vec![0_u64; 2];
        unsafe { pap.initialize_header(pap_nursery.as_mut_ptr().cast()) };
        let pap_root = pap_nursery.as_ptr() as usize | usize::from(pap.tag());
        let pap_heap =
            ObservationHeap::new(&pap_nursery, &statics, vec![pap], &function_constructors)
                .unwrap();
        assert!(matches!(
            pap_heap.observe_results(&[pap_root as u64], &reps, &layout, 1),
            Err(ObservationFailure::Unobservable(ObjectKind::Pap))
        ));

        let address_reps = [RuntimeRep::Address];
        let address_layout = StorageLayout::for_reps(&target(), &address_reps).unwrap();
        assert!(matches!(
            function_heap.observe_results(&[0], &address_reps, &address_layout, 1),
            Err(ObservationFailure::Representation(RuntimeRep::Address))
        ));
    }

    #[test]
    fn constructor_child_errors_are_reported_in_source_order() {
        let statics = statics();
        let parent_layout =
            StorageLayout::for_reps(&target(), &[RuntimeRep::Address, RuntimeRep::LiftedRef])
                .unwrap();
        let parent = Arc::new(ObjectDescriptor::constructor(1, parent_layout, None).unwrap());
        let function_layout = StorageLayout::for_reps(&target(), &[]).unwrap();
        let function =
            Arc::new(ObjectDescriptor::new(ObjectKind::Function, function_layout, None).unwrap());
        let mut nursery = vec![0_u64; 5];
        unsafe {
            parent.initialize_header(nursery.as_mut_ptr().cast());
            function.initialize_header(nursery.as_mut_ptr().add(3).cast());
        }
        nursery[2] = (nursery.as_ptr() as usize + 3 * 8 | usize::from(function.tag())) as u64;
        let constructors = BTreeMap::from([(
            parent.initial_header_word(),
            ConstructorObservation {
                identity: DataConId(30),
                fields: vec![RuntimeRep::Address, RuntimeRep::LiftedRef],
            },
        )]);
        let heap = ObservationHeap::new(&nursery, &statics, vec![parent, function], &constructors)
            .unwrap();
        let reps = [RuntimeRep::LiftedRef];
        let layout = StorageLayout::for_reps(&target(), &reps).unwrap();
        let root = nursery.as_ptr() as usize | 1;
        assert!(matches!(
            heap.observe_results(&[root as u64], &reps, &layout, 3),
            Err(ObservationFailure::Representation(RuntimeRep::Address))
        ));
    }
}
