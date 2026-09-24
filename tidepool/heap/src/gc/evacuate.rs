//! Evacuation: copying the graph reachable from one reference out of a live
//! machine's heap into a detached, sealed [`Parcel`], and later copying that
//! parcel into another machine's heap.
//!
//! Both halves are the ordinary Cheney copier (`raw.rs`). What differs from a
//! collection: the source keeps running afterwards, so every forwarding word
//! the export writes into the source is logged and restored before the
//! parcel is handed out; the destination is sized for the reachable subgraph,
//! not the whole source; and external payloads (byte arrays, boxed arrays)
//! are copied into parcel-owned storage so the parcel carries everything the
//! graph needs except static regions and code, which every machine that
//! installed the same image shares by address.
//!
//! A parcel never contains a continuation or an object under evaluation:
//! [`Parcel::check_transferable`] refuses both, so a value crosses machines
//! only at a quiescent point.

use crate::descriptor_region::{DescriptorArena, DescriptorOldSpace, DescriptorSourceSpace};
use crate::execution_descriptor::{
    DescriptorState, DescriptorTraceError, ObjectDescriptor, ObjectKind,
};
use crate::external_storage::{
    ExternalPayloadOwner, ExternalPointerSlots, ExternalStorageKind, ExternalStorageValidationError,
};
use crate::gc::raw::{
    copy_prevalidated_descriptor_graph_from_space, copy_reachable_from_space,
    prepare_descriptor_copy_from_space, prepare_reachable_copy_from_space, CopyResult,
    DescriptorSpace,
};
use crate::managed_reference::untag;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

/// The shape of one external payload as its owning ledger records it, so an
/// export can copy exactly the published bytes.
#[derive(Clone, Copy, Debug)]
pub struct PayloadShape {
    pub kind: ExternalStorageKind,
    pub logical_len: usize,
    /// Alignment of the byte payload; boxed arrays are pointer aligned.
    pub align: usize,
}

/// A ledger that can describe its payloads for export. The data itself is
/// read from the published pointer: the length prefix at `published`, then
/// `logical_len` bytes (Bytes) or `logical_len` pointer slots (BoxedArray).
pub trait PayloadExporter {
    fn shape(
        &self,
        published: *mut u8,
        expected: ExternalStorageKind,
    ) -> Result<PayloadShape, ExternalStorageValidationError>;
}

/// One external payload a parcel owns: the same `[len][data...]` layout a
/// machine ledger publishes, so the copier can expand it like any payload.
pub struct ParcelPayload {
    shape: PayloadShape,
    words: Box<[u64]>,
}

impl ParcelPayload {
    fn data_words(shape: PayloadShape) -> Result<usize, DescriptorTraceError> {
        let bytes = match shape.kind {
            ExternalStorageKind::Bytes => shape.logical_len,
            ExternalStorageKind::BoxedArray => shape
                .logical_len
                .checked_mul(std::mem::size_of::<*mut u8>())
                .ok_or(DescriptorTraceError::InvalidRange)?,
        };
        Ok(bytes.div_ceil(8))
    }

    /// Copy a payload out of the source ledger's published storage.
    ///
    /// # Safety
    /// `published` names a live payload of `shape` in the source ledger.
    unsafe fn copy_from(
        published: *mut u8,
        shape: PayloadShape,
    ) -> Result<Self, DescriptorTraceError> {
        let data = Self::data_words(shape)?;
        let mut words = Vec::new();
        words
            .try_reserve_exact(data + 1)
            .map_err(|_| DescriptorTraceError::MetadataAllocation)?;
        words.resize(data + 1, 0);
        words[0] = shape.logical_len as u64;
        let bytes = match shape.kind {
            ExternalStorageKind::Bytes => shape.logical_len,
            ExternalStorageKind::BoxedArray => data * 8,
        };
        std::ptr::copy_nonoverlapping(
            published.add(8),
            words.as_mut_ptr().add(1).cast::<u8>(),
            bytes,
        );
        Ok(Self {
            shape,
            words: words.into_boxed_slice(),
        })
    }

    pub fn shape(&self) -> PayloadShape {
        self.shape
    }

    /// The parcel-owned published pointer: the length prefix, data after it.
    pub fn published(&self) -> *mut u8 {
        self.words.as_ptr().cast_mut().cast()
    }

    /// The payload bytes after the length prefix, `logical_len` bytes for a
    /// byte array or `logical_len` pointer-sized slots for a boxed array.
    pub fn data(&self) -> &[u8] {
        let bytes = match self.shape.kind {
            ExternalStorageKind::Bytes => self.shape.logical_len,
            ExternalStorageKind::BoxedArray => self.shape.logical_len * 8,
        };
        // SAFETY: `words` holds the prefix word plus at least `bytes` bytes.
        unsafe { std::slice::from_raw_parts(self.words.as_ptr().add(1).cast::<u8>(), bytes) }
    }

    fn slots(&self) -> ExternalPointerSlots {
        let count = match self.shape.kind {
            ExternalStorageKind::Bytes => 0,
            ExternalStorageKind::BoxedArray => self.shape.logical_len,
        };
        // SAFETY: the boxed words after the prefix are `count` aligned slots
        // owned by this payload for its whole life.
        unsafe {
            ExternalPointerSlots::from_validated(
                self.words.as_ptr().cast_mut().add(1).cast::<*mut u8>(),
                count,
            )
        }
    }
}

/// A sealed, self-contained copy of one reachable graph, detached from every
/// machine. Static references inside it are shared addresses; everything
/// else lives in the parcel's own arena and payloads. Sent between threads
/// as a value; imported once.
pub struct Parcel {
    arena: DescriptorArena,
    /// The evacuated root: a tagged address inside `arena`, a shared static
    /// address, or 0 for a null reference.
    root: usize,
    payloads: Vec<ParcelPayload>,
    externals: usize,
}

// SAFETY: a parcel's arena and payloads are owned allocations no machine
// references; descriptors are shared immutable `Arc`s. Nothing reads or
// writes the parcel between its export and its import.
unsafe impl Send for Parcel {}

impl Parcel {
    pub fn root(&self) -> usize {
        self.root
    }

    pub fn bytes(&self) -> usize {
        self.arena.bytes_used()
    }

    pub fn payloads(&self) -> &[ParcelPayload] {
        &self.payloads
    }

    /// Number of external objects in the arena, the copier's payload
    /// scratch bound on import.
    pub fn externals(&self) -> usize {
        self.externals
    }

    /// Every descriptor an object in this parcel names, so an importer can
    /// check that it installed the images they belong to.
    pub fn descriptor_headers(&self) -> Result<Vec<usize>, DescriptorTraceError> {
        let mut headers = Vec::new();
        self.arena.walk_sealed(|_, descriptor| {
            let header = descriptor.initial_header_word();
            if !headers.contains(&header) {
                headers
                    .try_reserve(1)
                    .map_err(|_| DescriptorTraceError::MetadataAllocation)?;
                headers.push(header);
            }
            Ok(())
        })?;
        Ok(headers)
    }

    /// Refuse a parcel that could not be a settled value: a continuation is a
    /// parked frame of its machine, and an object under evaluation belongs
    /// to a running thread.
    pub fn check_transferable(&self) -> Result<(), DescriptorTraceError> {
        let range = self.arena.allocation_range();
        let used = self.arena.bytes_used();
        self.arena.walk_sealed(|object, descriptor| {
            let address = object as usize;
            if descriptor.kind() == ObjectKind::Continuation {
                return Err(DescriptorTraceError::ContinuationInParcel { address });
            }
            // SAFETY: walk_sealed proved the object start and extent.
            let state = unsafe { descriptor.state(object, used - (address - range.start)) }?;
            if state == DescriptorState::Evaluating {
                return Err(DescriptorTraceError::EvaluatingInParcel { address });
            }
            Ok(())
        })
    }

    /// Point every external object in the arena at the payload `map` names
    /// for its current published pointer. The importer calls this after it
    /// registered each parcel payload with the destination ledger, so the
    /// copier expands payloads the destination authenticates.
    ///
    /// # Safety
    /// The parcel is sealed and no copy is in progress.
    pub unsafe fn rewrite_payloads(
        &mut self,
        map: &HashMap<usize, *mut u8>,
    ) -> Result<(), DescriptorTraceError> {
        rewrite_payload_slots(&self.arena, map)
    }

    /// Copy this parcel's graph into `destination` (an arena the importing
    /// machine reserved for `self.bytes()` with its own descriptors), and
    /// return the root as a reference into that arena. `admitted` and
    /// `external` are the destination machine's; the parcel's arena is the
    /// consumed source and must not be imported twice.
    ///
    /// # Safety
    /// The destination machine is quiescent and exclusively borrowed;
    /// `rewrite_payloads` already pointed the parcel at payloads the
    /// destination ledger owns.
    pub unsafe fn import_into(
        &mut self,
        destination: &mut DescriptorArena,
        descriptors: &mut DescriptorSpace,
        admitted: Option<&dyn DescriptorOldSpace>,
        external: &dyn ExternalPayloadOwner,
    ) -> Result<(usize, CopyResult), DescriptorTraceError> {
        let mut root: *mut u8 = self.root as *mut u8;
        let root_ptrs = [&mut root as *mut *mut u8];
        prepare_descriptor_copy_from_space(
            &root_ptrs,
            &self.arena,
            self.externals,
            destination.destination(),
            descriptors,
        )?;
        let copied = copy_prevalidated_descriptor_graph_from_space(
            &root_ptrs,
            &self.arena,
            destination.destination(),
            descriptors,
            admitted,
            Some(external),
        )?;
        destination.seal(copied.bytes_copied)?;
        Ok((root as usize, copied))
    }
}

/// Rewrite the published payload pointer of every external object in
/// `arena` through `map`; an external object whose pointer the map does not
/// name is an integrity failure.
unsafe fn rewrite_payload_slots(
    arena: &DescriptorArena,
    map: &HashMap<usize, *mut u8>,
) -> Result<(), DescriptorTraceError> {
    arena.walk_sealed(|object, descriptor| {
        if descriptor.external_kind().is_none() {
            return Ok(());
        }
        let extent = descriptor.allocation_extent() as usize;
        let slot = descriptor.external_payload_slot(object, extent)?;
        let published = std::ptr::read(slot) as usize;
        let replacement = map
            .get(&published)
            .ok_or(DescriptorTraceError::MetadataIntegrity)?;
        std::ptr::write(slot, *replacement);
        Ok(())
    })
}

/// The payload owner an export hands the copier: every source payload it is
/// asked about is copied into parcel storage first, and the copier then
/// relocates the slots of the copy, never the source's.
struct ExportPayloads<'a> {
    exporter: &'a dyn PayloadExporter,
    copies: RefCell<Vec<ParcelPayload>>,
    index: RefCell<HashMap<usize, usize>>,
    failure: RefCell<Option<DescriptorTraceError>>,
}

impl ExportPayloads<'_> {
    fn copy_or_find(
        &self,
        published: *mut u8,
        expected: ExternalStorageKind,
    ) -> Result<usize, ExternalStorageValidationError> {
        if let Some(&index) = self.index.borrow().get(&(published as usize)) {
            return Ok(index);
        }
        let shape = self.exporter.shape(published, expected)?;
        // SAFETY: the exporter authenticated `published` as a live payload of
        // this shape in the source ledger, which no mutator touches during
        // the export.
        let copy = unsafe { ParcelPayload::copy_from(published, shape) }.map_err(|error| {
            *self.failure.borrow_mut() = Some(error);
            ExternalStorageValidationError::BookkeepingAllocation
        })?;
        let mut copies = self.copies.borrow_mut();
        copies
            .try_reserve(1)
            .map_err(|_| ExternalStorageValidationError::BookkeepingAllocation)?;
        let index = copies.len();
        copies.push(copy);
        self.index
            .borrow_mut()
            .try_reserve(1)
            .map_err(|_| ExternalStorageValidationError::BookkeepingAllocation)?;
        self.index.borrow_mut().insert(published as usize, index);
        Ok(index)
    }

    fn map(&self) -> HashMap<usize, *mut u8> {
        let copies = self.copies.borrow();
        self.index
            .borrow()
            .iter()
            .map(|(&published, &index)| (published, copies[index].published()))
            .collect()
    }
}

// SAFETY: the returned slots live in parcel-owned boxed storage that is
// neither freed nor resized while the copy borrows this owner; one published
// source pointer always maps to the same copy.
unsafe impl ExternalPayloadOwner for ExportPayloads<'_> {
    fn slots(
        &self,
        published: *mut u8,
        expected: ExternalStorageKind,
    ) -> Result<ExternalPointerSlots, ExternalStorageValidationError> {
        let index = self.copy_or_find(published, expected)?;
        Ok(self.copies.borrow()[index].slots())
    }
}

/// The smallest destination an export tries before growing.
const INITIAL_PARCEL_BYTES: usize = 64 * 1024;

/// Export the graph reachable from `root` (a tagged managed reference into
/// `source`, a static address, or 0) into a fresh parcel.
///
/// `descriptors` is the source machine's descriptor space (its static
/// catalog resolves static references; its scratch drives the copy);
/// `arena_descriptors` pins every layout the parcel may contain; `exporter`
/// describes source payloads. The destination starts small and doubles on
/// `InsufficientSpace`; every attempt restores the source's headers before
/// the next, so a failed export leaves the source exactly as it was.
///
/// # Safety
/// The source machine is quiescent and exclusively borrowed for the whole
/// call: no generated frame is live and no mutator runs.
pub unsafe fn export_reachable(
    root: usize,
    source: &dyn DescriptorSourceSpace,
    external_handles: usize,
    descriptors: &mut DescriptorSpace,
    arena_descriptors: &[Arc<ObjectDescriptor>],
    exporter: &dyn PayloadExporter,
) -> Result<Parcel, DescriptorTraceError> {
    let mut capacity = INITIAL_PARCEL_BYTES.min(source.source_bytes().max(16));
    loop {
        let payloads = ExportPayloads {
            exporter,
            copies: RefCell::new(Vec::new()),
            index: RefCell::new(HashMap::new()),
            failure: RefCell::new(None),
        };
        let mut arena = DescriptorArena::reserve(capacity, arena_descriptors.iter().cloned())?;
        let mut relocated: *mut u8 = root as *mut u8;
        let root_ptrs = [&mut relocated as *mut *mut u8];
        prepare_reachable_copy_from_space(
            &root_ptrs,
            source,
            external_handles,
            arena.destination(),
            descriptors,
        )?;
        // Each forwarded object occupies at least 16 destination bytes, plus
        // the thunks an updated chain forwards without copying.
        let objects = capacity / 16 + source.source_bytes() / 16 + 1;
        descriptors.begin_forwarding_log(objects)?;
        let outcome = copy_reachable_from_space(
            &root_ptrs,
            source,
            arena.destination(),
            descriptors,
            None,
            Some(&payloads),
        );
        descriptors.restore_forwarding_log();
        match outcome {
            Ok(copied) => {
                arena.seal(copied.bytes_copied)?;
                let map = payloads.map();
                rewrite_payload_slots(&arena, &map)?;
                let mut externals = 0;
                arena.walk_sealed(|_, descriptor| {
                    externals += usize::from(descriptor.external_kind().is_some());
                    Ok(())
                })?;
                let parcel = Parcel {
                    arena,
                    root: relocated as usize,
                    payloads: payloads.copies.into_inner(),
                    externals,
                };
                parcel.check_transferable()?;
                return Ok(parcel);
            }
            Err(DescriptorTraceError::InsufficientSpace { .. }) => {
                capacity = capacity
                    .checked_mul(2)
                    .ok_or(DescriptorTraceError::InvalidRange)?;
            }
            Err(DescriptorTraceError::ExternalPayload(
                ExternalStorageValidationError::BookkeepingAllocation,
            )) if payloads.failure.borrow().is_some() => {
                return Err(payloads
                    .failure
                    .into_inner()
                    .unwrap_or(DescriptorTraceError::MetadataAllocation));
            }
            Err(error) => return Err(error),
        }
    }
}

/// The live nursery of a machine as a copy source: exact object starts found
/// by one walk, so the copier can locate any nursery pointer.
pub struct NurseryView {
    start: usize,
    used: usize,
    starts: Vec<u64>,
}

impl NurseryView {
    /// Walk the initialized nursery prefix once, proving every object start
    /// against `descriptors`, and count its external objects.
    ///
    /// # Safety
    /// `start..start+used` is the machine's initialized nursery prefix and
    /// `descriptors` pins every layout in it.
    pub unsafe fn walk(
        descriptors: &DescriptorSpace,
        start: *const u8,
        used: usize,
    ) -> Result<(Self, usize), DescriptorTraceError> {
        if !used.is_multiple_of(8) {
            return Err(DescriptorTraceError::InvalidRange);
        }
        let mut starts = Vec::new();
        let words = (used / 8).div_ceil(64);
        starts
            .try_reserve_exact(words)
            .map_err(|_| DescriptorTraceError::MetadataAllocation)?;
        starts.resize(words, 0);
        let mut externals = 0;
        let mut offset = 0;
        while offset < used {
            let object = start.add(offset);
            let header = std::ptr::read(object.cast::<usize>()) & !7;
            let descriptor = descriptors
                .live_descriptor(header)
                .ok_or(DescriptorTraceError::UnknownDescriptor { address: header })?;
            let extent = descriptor.allocation_extent() as usize;
            if extent < 16 || !extent.is_multiple_of(8) || extent > used - offset {
                return Err(DescriptorTraceError::InvalidRange);
            }
            starts[offset / 8 / 64] |= 1 << (offset / 8 % 64);
            externals += usize::from(descriptor.external_kind().is_some());
            offset += extent;
        }
        Ok((
            Self {
                start: start as usize,
                used,
                starts,
            },
            externals,
        ))
    }
}

/// Everything a machine's graph can live in besides static regions: its
/// nursery and its old-space arenas. The export source.
pub struct MachineSpaces<'a> {
    pub nursery: NurseryView,
    pub arenas: &'a [DescriptorArena],
}

// SAFETY: the machine is quiescent and exclusively borrowed for the export;
// the nursery walk proved every start and extent, and the arenas' sealed
// metadata proves theirs. Neither moves during the copy.
unsafe impl DescriptorSourceSpace for MachineSpaces<'_> {
    fn locate_start(&self, address: usize) -> Result<Option<usize>, DescriptorTraceError> {
        let nursery = &self.nursery;
        if address >= nursery.start && address < nursery.start + nursery.used {
            let offset = address - nursery.start;
            if !offset.is_multiple_of(8)
                || nursery.starts[offset / 8 / 64] & (1 << (offset / 8 % 64)) == 0
            {
                return Err(DescriptorTraceError::InvalidManagedPointer { address });
            }
            return Ok(Some(nursery.used - offset));
        }
        for arena in self.arenas {
            if let Some(available) = arena.locate_start(address)? {
                return Ok(Some(available));
            }
        }
        Ok(None)
    }

    fn covers_slot(&self, address: usize) -> bool {
        self.overlaps_range(
            address,
            address.saturating_add(std::mem::size_of::<*mut u8>()),
        )
    }

    fn overlaps_range(&self, start: usize, end: usize) -> bool {
        let nursery = &self.nursery;
        if start < nursery.start + nursery.used && nursery.start < end {
            return true;
        }
        self.arenas.iter().any(|arena| {
            let range = arena.allocation_range();
            start < range.end && range.start < end
        })
    }

    fn source_bytes(&self) -> usize {
        self.nursery.used
            + self
                .arenas
                .iter()
                .map(DescriptorArena::bytes_used)
                .sum::<usize>()
    }
}

/// Whether `encoded` names an address inside `spaces` at all (not whether it
/// is an exact start); an exported root outside both is static or null.
pub fn names_machine_storage(spaces: &MachineSpaces<'_>, encoded: usize) -> bool {
    let address = untag(encoded);
    spaces.covers_slot(address)
}
