//! Closed immutable prepared objects, instantiated once per invocation.
//!
//! Only image-relative managed relocations may enter this owner. No mutable
//! payload API is exposed: skipping these objects during GC depends on both
//! graph closure and immutability, not just their address range.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use tidepool_repr::execution_schema::ValueId;

use crate::execution_descriptor::{DescriptorState, DescriptorTraceError, ObjectDescriptor, ObjectKind};
use crate::managed_reference::{tag_of, tag_valid, untag};

#[derive(Clone, Copy, Debug)]
pub struct StaticRelocation {
    pub slot_offset: usize,
    pub target_offset: usize,
    pub tag: u8,
}

#[derive(Debug, thiserror::Error)]
pub enum StaticImageError {
    #[error("static image allocation failed")]
    Allocation,
    #[error("invalid static object at byte offset {0}")]
    Object(usize),
    #[error("invalid or missing managed relocation at byte offset {0}")]
    Relocation(usize),
    #[error("static entry {0:?} is not an object start")]
    Entry(ValueId),
}

pub struct StaticImage {
    words: Vec<u64>,
    relocations: Vec<StaticRelocation>,
    entries: BTreeMap<ValueId, usize>,
    descriptors: BTreeMap<usize, Arc<ObjectDescriptor>>,
    starts: Vec<u64>,
}

/// Owns a stable allocation. Only the image can construct one, after all
/// relocation has completed. The descriptor owners outlive every header.
pub struct StaticRegion {
    words: Box<[u64]>,
    descriptors: BTreeMap<usize, Arc<ObjectDescriptor>>,
    starts: Vec<u64>,
    entries: BTreeMap<ValueId, usize>,
}

impl StaticImage {
    /// Headers name pinned descriptors. Every managed payload word must be zero
    /// in the image and have exactly one relocation to another exact start.
    /// Raw address/scalar words are never interpreted as relocations.
    pub fn new(
        words: Vec<u64>,
        relocations: Vec<StaticRelocation>,
        entries: BTreeMap<ValueId, usize>,
        descriptors: impl IntoIterator<Item = Arc<ObjectDescriptor>>,
    ) -> Result<Self, StaticImageError> {
        let descriptors: BTreeMap<_, _> = descriptors.into_iter()
            .map(|descriptor| (descriptor.initial_header_word(), descriptor)).collect();
        let bytes = words.len().checked_mul(8).ok_or(StaticImageError::Allocation)?;
        let mut starts = Vec::new();
        let bitmap_len = words.len().div_ceil(64);
        starts.try_reserve_exact(bitmap_len).map_err(|_| StaticImageError::Allocation)?;
        starts.resize(bitmap_len, 0_u64);
        let mut slots = BTreeSet::new();
        let mut offset = 0;
        while offset < bytes {
            let descriptor = descriptors.get(&(words[offset / 8] as usize))
                .ok_or(StaticImageError::Object(offset))?;
            let extent = descriptor.allocation_extent() as usize;
            if !matches!(descriptor.kind(), ObjectKind::Constructor | ObjectKind::Function)
                || extent < 16 || extent % 8 != 0 || extent > bytes - offset
                || descriptor.allocation_alignment() != 8
            {
                return Err(StaticImageError::Object(offset));
            }
            starts[offset / 8 / 64] |= 1_u64 << (offset / 8 % 64);
            for field in descriptor.trace_offsets() {
                let slot = offset + *field as usize;
                if *field as usize % 8 != 0 || *field as usize + 8 > extent
                    || words[slot / 8] != 0 || !slots.insert(slot)
                {
                    return Err(StaticImageError::Relocation(slot));
                }
            }
            offset += extent;
        }
        for relocation in &relocations {
            if !slots.remove(&relocation.slot_offset)
                || !is_start(&starts, bytes, relocation.target_offset)
            {
                return Err(StaticImageError::Relocation(relocation.slot_offset));
            }
            let header = words[relocation.target_offset / 8] as usize;
            let descriptor = &descriptors[&header];
            if !tag_valid(relocation.tag, descriptor.kind(), DescriptorState::Live, descriptor.constructor_tag()) {
                return Err(StaticImageError::Relocation(relocation.slot_offset));
            }
        }
        if let Some(slot) = slots.first() {
            return Err(StaticImageError::Relocation(*slot));
        }
        for (&id, &offset) in &entries {
            if !is_start(&starts, bytes, offset) {
                return Err(StaticImageError::Entry(id));
            }
        }
        Ok(Self { words, relocations, entries, descriptors, starts })
    }

    pub fn instantiate(&self) -> Result<StaticRegion, StaticImageError> {
        // wave4:STATIC_REGION — allocate aligned owned words fallibly, apply
        // every relocation (base + target | tag), then publish entries as
        // tagged addresses using each entry descriptor's canonical tag.
        todo!("wave4:STATIC_REGION")
    }
}

fn is_start(bitmap: &[u64], bytes: usize, offset: usize) -> bool {
    offset < bytes && offset % 8 == 0
        && bitmap[offset / 8 / 64] & (1_u64 << (offset / 8 % 64)) != 0
}

impl StaticRegion {
    pub fn entry(&self, id: ValueId) -> Option<usize> {
        self.entries.get(&id).copied()
    }

    /// Return None only for addresses outside this allocation. An interior or
    /// contradictory tagged pointer inside it is an integrity error.
    pub fn admit(&self, encoded: usize) -> Result<Option<usize>, DescriptorTraceError> {
        let base = self.words.as_ptr() as usize;
        let address = untag(encoded);
        let Some(offset) = address.checked_sub(base) else { return Ok(None); };
        let bytes = std::mem::size_of_val(self.words.as_ref());
        if offset >= bytes { return Ok(None); }
        if !is_start(&self.starts, bytes, offset) {
            return Err(DescriptorTraceError::InvalidManagedPointer { address });
        }
        let descriptor = &self.descriptors[&(self.words[offset / 8] as usize)];
        let tag = tag_of(encoded);
        if !tag_valid(tag, descriptor.kind(), DescriptorState::Live, descriptor.constructor_tag()) {
            return Err(DescriptorTraceError::InvalidManagedTag { address, tag });
        }
        Ok(Some(encoded))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_image_instantiates_without_publishing_entries() {
        let image = StaticImage::new(vec![], vec![], BTreeMap::new(), []).unwrap();
        let region = image.instantiate().unwrap();
        assert_eq!(region.entry(ValueId(0)), None);
        assert_eq!(region.admit(0).unwrap(), None);
    }
}
