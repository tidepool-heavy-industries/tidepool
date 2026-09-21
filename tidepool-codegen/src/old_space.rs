//! Stable roots and descriptor arenas retained by the prepared-STG machine.
//!
//! Prepared values move during collection, so every binding and handle keeps
//! the address of a persistently registered pointer cell. The collector
//! rewrites that cell in place and consumers always load through [`RootSlot`].

mod prepared;

pub(crate) use prepared::PreparedCompactionStats;

use std::collections::{HashMap, HashSet, VecDeque};
use tidepool_heap::layout::{
    read_size, read_tag, HeapTag, LitTag, CLOSURE_CAPTURED_OFFSET, CLOSURE_NUM_CAPTURED_OFFSET,
    CON_FIELDS_OFFSET, CON_NUM_FIELDS_OFFSET, FIELD_STRIDE, LIT_TAG_OFFSET, LIT_VALUE_OFFSET,
    TAG_FORWARDED, THUNK_BLACKHOLE, THUNK_CAPTURED_OFFSET, THUNK_EVALUATED,
    THUNK_INDIRECTION_OFFSET, THUNK_MIN_SIZE, THUNK_STATE_OFFSET, THUNK_UNEVALUATED,
};

use crate::machine_state::ExternalStorageKind;

/// Pointer slots and external payloads reachable in one packed nursery.
pub(crate) struct HeapRegionReachability {
    pub(crate) external_storage: HashMap<*mut u8, ExternalStorageKind>,
}

/// The stable, GC-updated slot holding a retained value's live heap pointer.
///
/// The slot address remains valid until the prepared machine drops. Collection
/// may rewrite the pointer stored in the slot, so consumers must call
/// [`Self::current`] instead of caching its value.
#[derive(Copy, Clone, Debug)]
pub struct RootSlot(*mut *mut u8);

impl RootSlot {
    /// Wrap a persistently registered pointer-cell address.
    ///
    /// # Safety
    /// `slot` must be non-null, valid, and registered as a persistent root for
    /// at least as long as this value can be reached.
    pub unsafe fn new(slot: *mut *mut u8) -> Self {
        Self(slot)
    }

    /// Load the collector-current heap pointer.
    ///
    /// # Safety
    /// The slot must still satisfy the invariant on [`Self::new`].
    pub unsafe fn current(self) -> *mut u8 {
        *self.0
    }

    /// Return the stable pointer-cell address.
    pub fn addr(self) -> *mut *mut u8 {
        self.0
    }
}

/// Prepared descriptor arenas and stable root cells owned by one invocation.
pub struct OldSpace {
    /// Heap cells whose addresses are registered as persistent roots.
    /// Boxing keeps each address stable if this vector reallocates.
    #[allow(clippy::vec_box)]
    pub(super) slots: Vec<Box<*mut u8>>,
    /// Descriptor arenas retained by invocation-local promotion.
    pub(crate) prepared_arenas: Vec<tidepool_heap::descriptor_region::DescriptorArena>,
}

// SAFETY: the owner moves only while quiescent; its raw pointers are accessed
// exclusively by the session's prepared-machine thread.
unsafe impl Send for OldSpace {}

impl Default for OldSpace {
    fn default() -> Self {
        Self::new()
    }
}

impl OldSpace {
    pub fn new() -> Self {
        Self {
            slots: Vec::new(),
            prepared_arenas: Vec::new(),
        }
    }

    /// Bytes retained in prepared descriptor arenas.
    pub fn prepared_bytes_used(&self) -> usize {
        self.prepared_arenas
            .iter()
            .map(tidepool_heap::descriptor_region::DescriptorArena::bytes_used)
            .sum()
    }
}
pub(crate) unsafe fn trace_heap_region(
    start: *mut u8,
    used: usize,
    root_slots: &[*mut *mut u8],
) -> Result<HeapRegionReachability, String> {
    let mut objects = HashMap::new();
    let mut forwarded = HashMap::new();
    let mut offset = 0usize;
    while offset < used {
        let pointer = unsafe { start.add(offset) };
        let size = unsafe { read_size(pointer) as usize };
        if size < 8 {
            return Err(format!("heap object at {pointer:p} has size {size}"));
        }
        let aligned = size
            .checked_add(7)
            .map(|n| n & !7)
            .ok_or_else(|| format!("heap object at {pointer:p} size overflows"))?;
        if offset + aligned > used {
            return Err(format!(
                "heap object at {pointer:p} extends past live prefix"
            ));
        }
        if unsafe { read_tag(pointer) } == TAG_FORWARDED {
            if size < 2 * std::mem::size_of::<usize>() {
                return Err(format!(
                    "forwarded nursery object at {pointer:p} is too small ({size})"
                ));
            }
            let target = unsafe { *(pointer.add(8) as *const *mut u8) };
            if target.is_null() {
                return Err(format!(
                    "forwarded nursery object at {pointer:p} has a null target"
                ));
            }
            forwarded.insert(pointer as usize, target);
        } else {
            unsafe { validate_object_shape(pointer, size)? };
            objects.insert(pointer as usize, size);
        }
        offset += aligned;
    }

    let end = unsafe { start.add(used) } as usize;
    let mut work = VecDeque::new();
    for &slot in root_slots {
        if slot.is_null() {
            return Err("null nursery root slot".into());
        }
        enqueue_region_slot(&objects, &forwarded, start as usize, end, slot, &mut work)?;
    }

    let mut live = HashSet::new();
    let mut external_storage = HashMap::new();
    while let Some(address) = work.pop_front() {
        if !live.insert(address) {
            continue;
        }
        let pointer = address as *mut u8;
        let size = objects[&address];
        for field_offset in unsafe { validated_pointer_offsets(pointer, size)? } {
            let slot = unsafe { pointer.add(field_offset) as *mut *mut u8 };
            enqueue_region_slot(&objects, &forwarded, start as usize, end, slot, &mut work)?;
        }
        if unsafe { read_tag(pointer) } == HeapTag::Lit.as_byte() {
            let tag = LitTag::from_byte(unsafe { *pointer.add(LIT_TAG_OFFSET) })
                .ok_or_else(|| format!("Lit at {pointer:p} has invalid tag"))?;
            if let Some(kind) = external_storage_kind(tag) {
                let payload = unsafe { *(pointer.add(LIT_VALUE_OFFSET) as *const *mut u8) };
                if !payload.is_null() {
                    insert_external_storage(&mut external_storage, payload as usize, kind)?;
                }
            }
        }
    }

    Ok(HeapRegionReachability {
        external_storage: external_storage
            .into_iter()
            .map(|(address, kind)| (address as *mut u8, kind))
            .collect(),
    })
}

fn enqueue_region_slot(
    objects: &HashMap<usize, usize>,
    forwarded: &HashMap<usize, *mut u8>,
    start: usize,
    end: usize,
    slot: *mut *mut u8,
    work: &mut VecDeque<usize>,
) -> Result<(), String> {
    let mut pointer = unsafe { *slot };
    let mut seen = HashSet::new();
    loop {
        let address = pointer as usize;
        if address < start || address >= end {
            unsafe { *slot = pointer };
            return Ok(());
        }
        if objects.contains_key(&address) {
            unsafe { *slot = pointer };
            work.push_back(address);
            return Ok(());
        }
        let Some(&target) = forwarded.get(&address) else {
            return Err(format!(
                "root graph points to non-object nursery address {pointer:p}"
            ));
        };
        if !seen.insert(address) {
            return Err(format!("nursery forwarding cycle begins at {pointer:p}"));
        }
        pointer = target;
    }
}

fn external_storage_kind(tag: LitTag) -> Option<ExternalStorageKind> {
    match tag {
        LitTag::String | LitTag::ByteArray => Some(ExternalStorageKind::Bytes),
        LitTag::SmallArray | LitTag::Array => Some(ExternalStorageKind::BoxedArray),
        _ => None,
    }
}

fn insert_external_storage(
    storage: &mut HashMap<usize, ExternalStorageKind>,
    pointer: usize,
    kind: ExternalStorageKind,
) -> Result<(), String> {
    if let Some(previous) = storage.insert(pointer, kind) {
        if previous != kind {
            return Err(format!(
                "external payload {pointer:#x} has conflicting wrapper kinds"
            ));
        }
    }
    Ok(())
}

unsafe fn validate_object_shape(pointer: *mut u8, size: usize) -> Result<(), String> {
    unsafe { validated_pointer_offsets(pointer, size) }.map(|_| ())
}

/// Return pointer-field offsets only after the tag-specific shape is proven to
/// fit the object's declared extent. This collector cannot use the GC's
/// best-effort scanner: malformed old-space metadata must abort staging rather
/// than silently under-trace a live graph.
unsafe fn validated_pointer_offsets(pointer: *mut u8, size: usize) -> Result<Vec<usize>, String> {
    let tag = HeapTag::from_byte(unsafe { read_tag(pointer) })
        .ok_or_else(|| format!("old-space object at {pointer:p} has invalid tag"))?;
    let field_offsets = |start: usize, count: usize| -> Result<Vec<usize>, String> {
        let end = count
            .checked_mul(FIELD_STRIDE)
            .and_then(|bytes| start.checked_add(bytes))
            .ok_or_else(|| format!("old-space object at {pointer:p} field extent overflows"))?;
        if end != size {
            return Err(format!(
                "old-space {tag} at {pointer:p} fields end at {end}, declared size is {size}"
            ));
        }
        Ok((0..count)
            .map(|index| start + index * FIELD_STRIDE)
            .collect())
    };

    match tag {
        HeapTag::Closure => {
            if size < CLOSURE_CAPTURED_OFFSET
                || size < CLOSURE_NUM_CAPTURED_OFFSET.saturating_add(2)
            {
                return Err(format!("old-space Closure at {pointer:p} is too small"));
            }
            let count =
                unsafe { *(pointer.add(CLOSURE_NUM_CAPTURED_OFFSET) as *const u16) as usize };
            field_offsets(CLOSURE_CAPTURED_OFFSET, count)
        }
        HeapTag::Con => {
            if size < CON_FIELDS_OFFSET || size < CON_NUM_FIELDS_OFFSET.saturating_add(2) {
                return Err(format!("old-space Con at {pointer:p} is too small"));
            }
            let count = unsafe { *(pointer.add(CON_NUM_FIELDS_OFFSET) as *const u16) as usize };
            field_offsets(CON_FIELDS_OFFSET, count)
        }
        HeapTag::Thunk => {
            if size < THUNK_MIN_SIZE || !(size - THUNK_CAPTURED_OFFSET).is_multiple_of(FIELD_STRIDE)
            {
                return Err(format!(
                    "old-space Thunk at {pointer:p} has invalid size {size}"
                ));
            }
            match unsafe { *pointer.add(THUNK_STATE_OFFSET) } {
                THUNK_UNEVALUATED | THUNK_BLACKHOLE => field_offsets(
                    THUNK_CAPTURED_OFFSET,
                    (size - THUNK_CAPTURED_OFFSET) / FIELD_STRIDE,
                ),
                THUNK_EVALUATED => Ok(vec![THUNK_INDIRECTION_OFFSET]),
                state => Err(format!(
                    "old-space Thunk at {pointer:p} has invalid state {state}"
                )),
            }
        }
        HeapTag::Lit => {
            if size != tidepool_heap::layout::LIT_SIZE {
                return Err(format!(
                    "old-space Lit at {pointer:p} has size {size}, expected {}",
                    tidepool_heap::layout::LIT_SIZE
                ));
            }
            let tag = unsafe { *pointer.add(LIT_TAG_OFFSET) };
            LitTag::from_byte(tag)
                .ok_or_else(|| format!("old-space Lit at {pointer:p} has invalid tag {tag}"))?;
            Ok(Vec::new())
        }
    }
}
