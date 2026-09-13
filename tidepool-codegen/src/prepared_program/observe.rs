//! Non-forcing, bounded materialization while invocation storage remains owned.

use std::collections::BTreeMap;
use std::sync::Arc;
use tidepool_bridge::Value;
use tidepool_heap::execution_descriptor::{DescriptorState, DescriptorTraceError, ObjectDescriptor, ObjectKind};
use tidepool_heap::managed_reference::{tag_of, tag_valid, untag};
use tidepool_heap::static_region::StaticRegion;
use tidepool_repr::execution_schema::{RuntimeRep, StorageLayout};
use super::ConstructorObservation;

#[derive(Debug, thiserror::Error)]
pub enum ObservationFailure {
    #[error("observation node budget {limit} exhausted")]
    BudgetExceeded { limit: usize },
    #[error("cannot observe {0:?} without forcing or applying it")]
    Unobservable(ObjectKind),
    #[error("representation {0:?} is not a materialized host value")]
    Representation(RuntimeRep),
    #[error(transparent)]
    Integrity(#[from] DescriptorTraceError),
}

pub(super) struct ObservationHeap<'a> {
    nursery: &'a [u64],
    statics: &'a StaticRegion,
    descriptors: BTreeMap<usize, Arc<ObjectDescriptor>>,
    starts: Vec<u64>,
    constructors: &'a BTreeMap<usize, ConstructorObservation>,
}

impl<'a> ObservationHeap<'a> {
    pub fn new(
        nursery: &'a [u64],
        statics: &'a StaticRegion,
        descriptors: impl IntoIterator<Item = Arc<ObjectDescriptor>>,
        constructors: &'a BTreeMap<usize, ConstructorObservation>,
    ) -> Result<Self, ObservationFailure> {
        let descriptors: BTreeMap<_, _> = descriptors.into_iter()
            .map(|descriptor| (descriptor.initial_header_word(), descriptor)).collect();
        let mut starts = Vec::new();
        starts.try_reserve_exact(nursery.len().div_ceil(64))
            .map_err(|_| DescriptorTraceError::MetadataAllocation)?;
        starts.resize(nursery.len().div_ceil(64), 0_u64);
        let mut offset = 0;
        while offset < nursery.len() {
            let header = nursery[offset] as usize;
            let descriptor = descriptors.get(&(header & !7))
                .ok_or(DescriptorTraceError::UnknownDescriptor { address: header & !7 })?;
            let available = (nursery.len() - offset) * 8;
            // The borrowed slice proves allocation bounds before any object read.
            let state = unsafe { descriptor.state(nursery.as_ptr().add(offset).cast(), available)? };
            if state != DescriptorState::Live {
                return Err(DescriptorTraceError::StateForKind { state, kind: descriptor.kind() }.into());
            }
            let extent = descriptor.allocation_extent() as usize;
            if extent < 16 || extent % 8 != 0 { return Err(DescriptorTraceError::InvalidRange.into()); }
            starts[offset / 64] |= 1_u64 << (offset % 64);
            offset += extent / 8;
        }
        Ok(Self { nursery, statics, descriptors, starts, constructors })
    }

    fn object(&self, encoded: usize) -> Result<(&ObjectDescriptor, *const u8), ObservationFailure> {
        let address = untag(encoded);
        let static_pointer = self.statics.admit(encoded)?.is_some();
        if !static_pointer {
            let base = self.nursery.as_ptr() as usize;
            let offset = address.checked_sub(base)
                .ok_or(DescriptorTraceError::InvalidManagedPointer { address })?;
            if offset % 8 != 0 || offset / 8 >= self.nursery.len()
                || self.starts[offset / 8 / 64] & (1_u64 << (offset / 8 % 64)) == 0 {
                return Err(DescriptorTraceError::InvalidManagedPointer { address }.into());
            }
        }
        // Exact-start membership was proved by a validated immutable region or
        // the nursery walk; both allocations remain borrowed through observation.
        let header = unsafe { std::ptr::read(address as *const usize) };
        let descriptor = self.descriptors.get(&header)
            .ok_or(DescriptorTraceError::UnknownDescriptor { address: header })?;
        if !tag_valid(tag_of(encoded), descriptor.kind(), DescriptorState::Live, descriptor.constructor_tag()) {
            return Err(DescriptorTraceError::InvalidManagedTag { address, tag: tag_of(encoded) }.into());
        }
        Ok((descriptor, address as *const u8))
    }

    /// Result storage is already registered as roots by the invocation owner.
    /// No forcing, native call, or collection occurs anywhere in this traversal.
    pub fn observe_results(
        &self,
        _words: &[u64],
        _reps: &[RuntimeRep],
        _layout: &StorageLayout,
        _budget: usize,
    ) -> Result<Vec<Value>, ObservationFailure> {
        // wave4:OBSERVE — fallible recursion-crate unfold, source-order child
        // observation, one shared node budget across results; Void omitted.
        // Constructor identity + logical reps come from self.constructors.
        // Raw Address/function/PAP/unexpected kind fail typed, never force.
        todo!("wave4:OBSERVE")
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn deep_observation_and_budget_cleanup_use_small_stack() {
        todo!("wave4:OBSERVE_TESTS")
    }
}
