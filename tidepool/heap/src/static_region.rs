//! Closed immutable prepared objects, instantiated once per invocation.
//!
//! Only image-relative managed relocations may enter this owner. No mutable
//! payload API is exposed: skipping these objects during GC depends on both
//! graph closure and immutability, not just their address range.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use tidepool_repr::execution_schema::ValueId;

use crate::execution_descriptor::{
    DescriptorState, DescriptorTraceError, ObjectDescriptor, ObjectKind,
};
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
    /// Arc-wrapped: `instantiate` hands its `StaticRegion` the same owner by
    /// `Arc::clone` rather than rebuilding the map/bitmap -- both are fixed
    /// once the image validates, and `instantiate` may run once per install.
    descriptors: Arc<BTreeMap<usize, Arc<ObjectDescriptor>>>,
    starts: Arc<[u64]>,
}

/// Owns a stable allocation. Only the image can construct one, after all
/// relocation has completed. The descriptor owners outlive every header.
pub struct StaticRegion {
    words: Box<[u64]>,
    descriptors: Arc<BTreeMap<usize, Arc<ObjectDescriptor>>>,
    starts: Arc<[u64]>,
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
        let descriptors: BTreeMap<_, _> = descriptors
            .into_iter()
            .map(|descriptor| (descriptor.initial_header_word(), descriptor))
            .collect();
        let bytes = words
            .len()
            .checked_mul(8)
            .ok_or(StaticImageError::Allocation)?;
        let mut starts = Vec::new();
        let bitmap_len = words.len().div_ceil(64);
        starts
            .try_reserve_exact(bitmap_len)
            .map_err(|_| StaticImageError::Allocation)?;
        starts.resize(bitmap_len, 0_u64);
        let mut slots = BTreeSet::new();
        let mut offset = 0;
        while offset < bytes {
            let descriptor = descriptors
                .get(&(words[offset / 8] as usize))
                .ok_or(StaticImageError::Object(offset))?;
            let extent = descriptor.allocation_extent() as usize;
            if !matches!(
                descriptor.kind(),
                ObjectKind::Constructor | ObjectKind::Function
            ) || extent < 16
                || !extent.is_multiple_of(8)
                || extent > bytes - offset
                || descriptor.allocation_alignment() != 8
            {
                return Err(StaticImageError::Object(offset));
            }
            starts[offset / 8 / 64] |= 1_u64 << (offset / 8 % 64);
            for field in descriptor.trace_offsets() {
                let slot = offset + *field as usize;
                if !(*field as usize).is_multiple_of(8)
                    || *field as usize + 8 > extent
                    || words[slot / 8] != 0
                    || !slots.insert(slot)
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
            if !tag_valid(
                relocation.tag,
                descriptor.kind(),
                DescriptorState::Live,
                descriptor.constructor_tag(),
            ) {
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
        Ok(Self {
            words,
            relocations,
            entries,
            descriptors: Arc::new(descriptors),
            starts: Arc::from(starts),
        })
    }

    pub fn instantiate(&self) -> Result<StaticRegion, StaticImageError> {
        // Allocate before exposing any address. The image has already checked
        // every relocation, so these writes cannot fail after allocation.
        let mut words = Vec::new();
        words
            .try_reserve_exact(self.words.len())
            .map_err(|_| StaticImageError::Allocation)?;
        words.extend_from_slice(&self.words);
        let mut words = words.into_boxed_slice();
        let base = words.as_mut_ptr() as usize;

        for relocation in &self.relocations {
            let target = base
                .checked_add(relocation.target_offset)
                .ok_or(StaticImageError::Relocation(relocation.slot_offset))?;
            let encoded = target
                .checked_add(usize::from(relocation.tag))
                .ok_or(StaticImageError::Relocation(relocation.slot_offset))?;
            words[relocation.slot_offset / 8] = encoded as u64;
        }

        let mut entries = BTreeMap::new();
        for (&id, &offset) in &self.entries {
            let address = base
                .checked_add(offset)
                .ok_or(StaticImageError::Entry(id))?;
            let descriptor = &self.descriptors[&(self.words[offset / 8] as usize)];
            let encoded = address
                .checked_add(usize::from(descriptor.tag()))
                .ok_or(StaticImageError::Entry(id))?;
            entries.insert(id, encoded);
        }

        Ok(StaticRegion {
            words,
            descriptors: Arc::clone(&self.descriptors),
            starts: Arc::clone(&self.starts),
            entries,
        })
    }
}

fn is_start(bitmap: &[u64], bytes: usize, offset: usize) -> bool {
    offset < bytes
        && offset.is_multiple_of(8)
        && bitmap[offset / 8 / 64] & (1_u64 << (offset / 8 % 64)) != 0
}

impl StaticRegion {
    /// Empty images have no stable address to classify. Their zero-length
    /// boxed slice may use a shared dangling pointer, so catalogs deliberately
    /// do not index or record them.
    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// Immutable allocation bounds, for rejecting collector root slots that
    /// would otherwise write into this region. Empty regions contain nothing.
    pub fn address_range(&self) -> std::ops::Range<usize> {
        let start = self.words.as_ptr() as usize;
        start..start + std::mem::size_of_val(self.words.as_ref())
    }

    pub fn entry(&self, id: ValueId) -> Option<usize> {
        self.entries.get(&id).copied()
    }

    /// Return None only for addresses outside this allocation. An interior or
    /// contradictory tagged pointer inside it is an integrity error.
    pub fn admit(&self, encoded: usize) -> Result<Option<usize>, DescriptorTraceError> {
        let base = self.words.as_ptr() as usize;
        let address = untag(encoded);
        let Some(offset) = address.checked_sub(base) else {
            return Ok(None);
        };
        let bytes = std::mem::size_of_val(self.words.as_ref());
        if offset >= bytes {
            return Ok(None);
        }
        if !is_start(&self.starts, bytes, offset) {
            return Err(DescriptorTraceError::InvalidManagedPointer { address });
        }
        let descriptor = &self.descriptors[&(self.words[offset / 8] as usize)];
        let tag = tag_of(encoded);
        if !tag_valid(
            tag,
            descriptor.kind(),
            DescriptorState::Live,
            descriptor.constructor_tag(),
        ) {
            return Err(DescriptorTraceError::InvalidManagedTag { address, tag });
        }
        Ok(Some(encoded))
    }
}

/// The immutable static allocations admitted by one prepared machine.
///
/// Regions are ordered by their stable allocation address.  The order is an
/// index only: a selected region still performs the exact object-start and
/// tag validation in [`StaticRegion::admit`].  That keeps an interior or
/// contradictorily tagged pointer an integrity error rather than letting a
/// range lookup grant it membership.
pub struct StaticRegionCatalog {
    regions: Vec<Arc<StaticRegion>>,
}

impl StaticRegionCatalog {
    pub fn new() -> Self {
        Self {
            regions: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.regions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    pub fn insert(&mut self, region: Arc<StaticRegion>) -> Result<bool, DescriptorTraceError> {
        if region.is_empty() {
            return Ok(false);
        }
        let start = region.address_range().start;
        match self
            .regions
            .binary_search_by_key(&start, |existing| existing.address_range().start)
        {
            Ok(index) => {
                if Arc::ptr_eq(&self.regions[index], &region) {
                    Ok(false)
                } else {
                    Err(DescriptorTraceError::MetadataIntegrity)
                }
            }
            Err(index) => {
                self.regions
                    .try_reserve(1)
                    .map_err(|_| DescriptorTraceError::MetadataAllocation)?;
                self.regions.insert(index, region);
                Ok(true)
            }
        }
    }

    pub fn remove(&mut self, region: &Arc<StaticRegion>) -> bool {
        if region.is_empty() {
            return false;
        }
        let start = region.address_range().start;
        let Ok(index) = self
            .regions
            .binary_search_by_key(&start, |existing| existing.address_range().start)
        else {
            return false;
        };
        if !Arc::ptr_eq(&self.regions[index], region) {
            return false;
        }
        self.regions.remove(index);
        true
    }

    pub fn remove_start(&mut self, start: usize) -> bool {
        let Ok(index) = self
            .regions
            .binary_search_by_key(&start, |existing| existing.address_range().start)
        else {
            return false;
        };
        self.regions.remove(index);
        true
    }

    /// Admit `encoded` only after a range search selects its one possible
    /// region.  Allocation ranges cannot overlap, so the predecessor of the
    /// untagged address is the sole candidate.
    pub fn admit(
        &self,
        encoded: usize,
        metrics: &StaticLookupMetrics,
    ) -> Result<Option<&StaticRegion>, DescriptorTraceError> {
        let address = untag(encoded);
        let candidate = self
            .regions
            .partition_point(|region| region.address_range().start <= address);
        let Some(region) = candidate
            .checked_sub(1)
            .and_then(|index| self.regions.get(index))
        else {
            metrics.record(self.len(), 0, false);
            return Ok(None);
        };
        let range = region.address_range();
        if address >= range.end {
            metrics.record(self.len(), 0, false);
            return Ok(None);
        }
        let admitted = region.admit(encoded);
        metrics.record(self.len(), 1, admitted.as_ref().is_ok_and(Option::is_some));
        admitted.map(|value| value.map(|_| region.as_ref()))
    }

    pub fn overlaps_slot(&self, address: usize) -> bool {
        let Some(end) = address.checked_add(std::mem::size_of::<*mut u8>()) else {
            return true;
        };
        let candidate = self
            .regions
            .partition_point(|region| region.address_range().start < end);
        candidate
            .checked_sub(1)
            .and_then(|index| self.regions.get(index))
            .is_some_and(|region| {
                let range = region.address_range();
                address < range.end
            })
    }
}

impl Default for StaticRegionCatalog {
    fn default() -> Self {
        Self::new()
    }
}

/// Opt-in lookup work, accumulated for one collector or observation view.
/// Disabled counters do not perform atomic operations on the pointer path.
pub struct StaticLookupMetrics {
    enabled: bool,
    owner: &'static str,
    calls: std::sync::atomic::AtomicUsize,
    probes: std::sync::atomic::AtomicUsize,
    hits: std::sync::atomic::AtomicUsize,
    max_regions: std::sync::atomic::AtomicUsize,
}

impl StaticLookupMetrics {
    pub fn new(owner: &'static str) -> Self {
        Self {
            enabled: std::env::var("TIDEPOOL_MEMORY_DETAIL").as_deref() == Ok("1"),
            owner,
            calls: Default::default(),
            probes: Default::default(),
            hits: Default::default(),
            max_regions: Default::default(),
        }
    }

    pub fn record(&self, regions: usize, probes: usize, hit: bool) {
        if self.enabled {
            use std::sync::atomic::Ordering::Relaxed;
            self.calls.fetch_add(1, Relaxed);
            self.probes.fetch_add(probes, Relaxed);
            self.hits.fetch_add(usize::from(hit), Relaxed);
            self.max_regions.fetch_max(regions, Relaxed);
        }
    }
}

impl Drop for StaticLookupMetrics {
    fn drop(&mut self) {
        if self.enabled {
            use std::sync::atomic::Ordering::Relaxed;
            eprintln!(
                "tidepool-static-lookup-lifetime owner={} lifetime_lookup_calls={} lifetime_region_probes={} lifetime_static_hits={} lifetime_max_regions={}",
                self.owner,
                self.calls.load(Relaxed),
                self.probes.load(Relaxed),
                self.hits.load(Relaxed),
                self.max_regions.load(Relaxed)
            );
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tidepool_repr::execution_schema::{
        Architecture, Endianness, RuntimeRep, StorageLayout, TargetDescriptor,
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

    fn descriptor(tag: u32, reps: &[RuntimeRep]) -> Arc<ObjectDescriptor> {
        Arc::new(
            ObjectDescriptor::constructor(
                tag,
                StorageLayout::for_reps(&target(), reps).unwrap(),
                None,
            )
            .unwrap(),
        )
    }

    fn image_with_one_object(
        descriptor: Arc<ObjectDescriptor>,
        entries: BTreeMap<ValueId, usize>,
        relocations: Vec<StaticRelocation>,
    ) -> StaticImage {
        let extent = descriptor.allocation_extent() as usize;
        let mut words = vec![0_u64; extent / 8];
        words[0] = descriptor.initial_header_word() as u64;
        StaticImage::new(words, relocations, entries, [descriptor]).unwrap()
    }

    #[test]
    fn empty_image_instantiates_without_publishing_entries() {
        let image = StaticImage::new(vec![], vec![], BTreeMap::new(), []).unwrap();
        let region = image.instantiate().unwrap();
        assert_eq!(region.entry(ValueId(0)), None);
        assert_eq!(region.admit(0).unwrap(), None);
    }

    #[test]
    fn catalog_ignores_multiple_empty_regions_with_shared_dangling_addresses() {
        let image = StaticImage::new(vec![], vec![], BTreeMap::new(), []).unwrap();
        let first = Arc::new(image.instantiate().unwrap());
        let second = Arc::new(image.instantiate().unwrap());
        assert_eq!(first.address_range().start, second.address_range().start);
        let mut catalog = StaticRegionCatalog::new();
        assert!(!catalog.insert(first).unwrap());
        assert!(!catalog.insert(second).unwrap());
        assert_eq!(catalog.len(), 0);
    }

    #[test]
    fn cyclic_pair_relocations_are_instantiated_with_target_tags() {
        let first = descriptor(1, &[RuntimeRep::LiftedRef]);
        let second = descriptor(2, &[RuntimeRep::LiftedRef]);
        assert_eq!(first.allocation_extent(), 16);
        assert_eq!(second.allocation_extent(), 16);
        let mut words = vec![0_u64; 4];
        words[0] = first.initial_header_word() as u64;
        words[2] = second.initial_header_word() as u64;
        let image = StaticImage::new(
            words,
            vec![
                StaticRelocation {
                    slot_offset: 8,
                    target_offset: 16,
                    tag: second.tag(),
                },
                StaticRelocation {
                    slot_offset: 24,
                    target_offset: 0,
                    tag: first.tag(),
                },
            ],
            BTreeMap::from([(ValueId(41), 0), (ValueId(9001), 16)]),
            [first, second],
        )
        .unwrap();
        let region = image.instantiate().unwrap();
        let base = region.words.as_ptr() as usize;
        assert_eq!(region.words[1] as usize, (base + 16) | 2);
        assert_eq!(region.words[3] as usize, base | 1);
        assert_eq!(
            region.admit(region.entry(ValueId(41)).unwrap()).unwrap(),
            Some(region.entry(ValueId(41)).unwrap())
        );
        assert_eq!(
            region.admit(region.entry(ValueId(9001)).unwrap()).unwrap(),
            Some(region.entry(ValueId(9001)).unwrap())
        );
    }

    #[test]
    fn relocation_validation_rejects_escaping_missing_and_duplicate_edges() {
        let descriptor = descriptor(1, &[RuntimeRep::LiftedRef]);
        let extent = descriptor.allocation_extent() as usize;
        let entries = BTreeMap::new();

        let mut words = vec![0_u64; extent / 8];
        words[0] = descriptor.initial_header_word() as u64;
        assert!(matches!(
            StaticImage::new(
                words.clone(),
                vec![StaticRelocation {
                    slot_offset: 8,
                    target_offset: extent,
                    tag: descriptor.tag(),
                }],
                entries.clone(),
                [descriptor.clone()],
            ),
            Err(StaticImageError::Relocation(8))
        ));
        assert!(matches!(
            StaticImage::new(words.clone(), vec![], entries.clone(), [descriptor.clone()]),
            Err(StaticImageError::Relocation(8))
        ));
        assert!(matches!(
            StaticImage::new(
                words,
                vec![
                    StaticRelocation {
                        slot_offset: 8,
                        target_offset: 0,
                        tag: descriptor.tag(),
                    },
                    StaticRelocation {
                        slot_offset: 8,
                        target_offset: 0,
                        tag: descriptor.tag(),
                    },
                ],
                entries,
                [descriptor],
            ),
            Err(StaticImageError::Relocation(8))
        ));
    }

    #[test]
    fn entries_are_keyed_by_full_value_id_and_invocations_are_distinct() {
        let descriptor = descriptor(1, &[]);
        let image = image_with_one_object(
            descriptor,
            BTreeMap::from([(ValueId(17), 0), (ValueId(4_000_000), 0)]),
            vec![],
        );
        let first = image.instantiate().unwrap();
        let second = image.instantiate().unwrap();
        assert_eq!(first.entry(ValueId(17)), first.entry(ValueId(4_000_000)));
        assert_ne!(first.entry(ValueId(17)), second.entry(ValueId(17)));
    }

    #[test]
    fn admission_rejects_interiors_and_contradictory_tags() {
        let image = image_with_one_object(
            descriptor(1, &[]),
            BTreeMap::from([(ValueId(73), 0)]),
            vec![],
        );
        let region = image.instantiate().unwrap();
        let entry = region.entry(ValueId(73)).unwrap();
        assert!(matches!(
            region.admit(untag(entry) + 8),
            Err(DescriptorTraceError::InvalidManagedPointer { .. })
        ));
        assert!(matches!(
            region.admit(entry | 2),
            Err(DescriptorTraceError::InvalidManagedTag { .. })
        ));
    }

    #[test]
    fn catalog_selects_ranges_but_keeps_exact_admission_authoritative() {
        let image = image_with_one_object(
            descriptor(1, &[]),
            BTreeMap::from([(ValueId(73), 0)]),
            vec![],
        );
        let first = Arc::new(image.instantiate().unwrap());
        let second = Arc::new(image.instantiate().unwrap());
        let mut catalog = StaticRegionCatalog::new();
        // Addresses are allocator-selected, so insertion must not rely on
        // installation order to find either region.
        catalog.insert(Arc::clone(&second)).unwrap();
        catalog.insert(Arc::clone(&first)).unwrap();
        let metrics = StaticLookupMetrics::new("test");
        let entry = first.entry(ValueId(73)).unwrap();
        assert!(std::ptr::eq(
            catalog.admit(entry, &metrics).unwrap().unwrap(),
            first.as_ref()
        ));
        assert!(matches!(
            catalog.admit(untag(entry) + 8, &metrics),
            Err(DescriptorTraceError::InvalidManagedPointer { .. })
        ));
        assert!(catalog.remove(&first));
        assert!(catalog.admit(entry, &metrics).unwrap().is_none());
    }
}

