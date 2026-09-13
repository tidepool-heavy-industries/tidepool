//! Old-space (gen-1) tenuring for the persistent binding store.
//!
//! The session heap is split into two generations:
//!
//! - **gen-0 (nursery):** the bump-allocated region the JIT allocates into;
//!   collected by the minor (Cheney) GC on every `gc_trigger`.
//! - **gen-1 (`OldSpace`):** an append-only, growable buffer holding *tenured*
//!   bindings — strict-forced, immutable Tier0 data and unforced Tier1
//!   closures, promoted out of the nursery once at bind time so a later run
//!   can resolve them through a stable [`RootSlot`].
//!
//! ## The write barrier
//!
//! Every store of a nursery pointer into already-tenured or external-to-
//! nursery memory routes through ONE function, [`crate::host_fns::write_barrier`]:
//! thunk memoization (a Tier1 closure tenures UNFORCED, and forcing it later
//! mutates its indirection cell to point at a nursery result),
//! `WriteSmallArray`/`WriteArray`, `casSmallArray#`, and the
//! boxed-array copy family's destination range, and `deep_force`'s constructor
//! field rewrites. `write_barrier` records the
//! store's destination slot in the machine's remembered set; `perform_gc`
//! traces and rewrites every remembered slot on every collection (including
//! the doubling re-evacuate, which reuses the same root-slot list), exactly
//! like a stack or persistent root. The barrier is armed on the first
//! `OldSpace::tenure` call — before that there is no old-space, so no
//! old-to-young store is possible.
//!
//! The minor GC's from-range is the nursery ONLY (`raw::cheney_copy`'s
//! `is_in_range` excludes old-space addresses), so tenured objects are never
//! scanned, moved, or evacuated by a minor collection, and their addresses
//! are stable for the session's life. Under `TIDEPOOL_HEAP_VERIFY` a second
//! pass (`host_fns::gc`'s `verify_tenured_graph`) walks the tenured graph
//! from the persistent roots and classifies every slot it reaches, including
//! a boxed array's external malloc'd payload slots. That pass follows the
//! object graph rather than the remembered set, so it is independent of this
//! barrier and detects a store the barrier failed to record — at the
//! collection that strands the target, rather than whenever something next
//! dereferences it.
//!
//! Old-space is compacted only on an explicit *major* pass (when a binding
//! generation dies) — never during a minor GC.
//!
//! ## Sibling-reference fixup
//!
//! The write barrier above covers stores made AFTER an object is tenured. It
//! does not cover a SIBLING object — some other live nursery value that
//! independently held a pointer into the graph [`OldSpace::tenure`] is about
//! to evacuate, from BEFORE that tenure call runs. `tenure`'s own
//! `cheney_copy` walk only visits the tenure root's own transitive graph;
//! `raw::evacuate` unconditionally overwrites a moved object's old nursery
//! address with a `TAG_FORWARDED` stub the instant it is copied, so a
//! sibling's untouched field reads that stub the moment it is next
//! dereferenced — not eventually, immediately. `tenure` closes this by
//! calling [`crate::host_fns::run_minor_collection_for_tenure_fixup`] right
//! after its own walk, whenever it actually evacuated something: a real
//! minor collection over every ordinary root category fixes up any sibling
//! reachable from those roots via the SAME forward-following logic this
//! module's own `test_overlapping_tenures_preserve_sharing` proves correct
//! for shared substructure across two tenure calls. See that function's doc
//! for the full mechanism and why it is safe to run from every tenure call
//! site.

#![allow(dead_code)]

use std::collections::{HashMap, HashSet, VecDeque};
use tidepool_heap::gc::raw::{cheney_copy, for_each_pointer_field};
use tidepool_heap::layout::{
    read_size, read_tag, HeapTag, LitTag, CLOSURE_CAPTURED_OFFSET, CLOSURE_NUM_CAPTURED_OFFSET,
    CON_FIELDS_OFFSET, CON_NUM_FIELDS_OFFSET, FIELD_STRIDE, LIT_TAG_OFFSET, LIT_VALUE_OFFSET,
    TAG_FORWARDED, THUNK_BLACKHOLE, THUNK_CAPTURED_OFFSET, THUNK_EVALUATED,
    THUNK_INDIRECTION_OFFSET, THUNK_MIN_SIZE, THUNK_STATE_OFFSET, THUNK_UNEVALUATED,
};

use crate::context::VMContext;
use crate::machine_state::ExternalStorageKind;

/// Default arena size: 1 MiB. Each arena is a contiguous allocation whose
/// byte-level address is stable for the OldSpace's lifetime.
const DEFAULT_ARENA: usize = 1 << 20;

/// The stable, GC-updated slot a tenured binding's live heap pointer lives in.
///
/// INVARIANT: the slot *address* is stable for the binding's life; the copying
/// GC rewrites `*slot` in place on every collection that relocates the value.
/// Value resolution LOADS THROUGH this slot — it must never snapshot the
/// pointer (a snapshot goes stale after an old-space compaction). The unsafe
/// accessor lives here so callers cannot fabricate a slot from a bare pointer.
///
/// (Domain model §4. A tenured value in old-space is not moved by minor GCs, so
/// its slot is effectively constant between major passes — but resolution still
/// loads through it for uniformity with nursery-resident roots.)
#[derive(Copy, Clone, Debug)]
pub struct RootSlot(*mut *mut u8);

impl RootSlot {
    /// Wrap a raw slot address. The caller asserts the slot is a valid,
    /// persistently-rooted `*mut *mut u8` for the machine's life.
    ///
    /// # Safety
    /// See the type-level invariant: `slot` must be non-null, valid, and
    /// registered as a persistent GC root until the session machine drops.
    pub unsafe fn new(slot: *mut *mut u8) -> Self {
        RootSlot(slot)
    }

    /// Load the GC-current heap pointer from the slot.
    ///
    /// # Safety
    /// The slot must be valid + registered as a persistent root (see the type
    /// invariant). Returns the live pointer, not a stale snapshot.
    pub unsafe fn current(self) -> *mut u8 {
        *self.0
    }

    /// The slot address itself — the immediate a Var-miss site `iconst`s and
    /// then `load`s through (per fragment). Stable for the machine's life.
    pub fn addr(self) -> *mut *mut u8 {
        self.0
    }
}

struct OldArena {
    words: Vec<u64>,
    used: usize,
}

impl OldArena {
    fn new(bytes: usize) -> Self {
        let words = bytes.saturating_add(7) / 8;
        Self {
            words: vec![0; words],
            used: 0,
        }
    }

    fn start(&self) -> *const u8 {
        self.words.as_ptr().cast()
    }

    fn start_mut(&mut self) -> *mut u8 {
        self.words.as_mut_ptr().cast()
    }

    fn capacity_bytes(&self) -> usize {
        self.words.len() * 8
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: u64 storage is contiguous, initialized and structurally
        // 8-byte aligned. The byte view covers its exact allocation.
        unsafe { std::slice::from_raw_parts_mut(self.start_mut(), self.capacity_bytes()) }
    }
}

/// A fully allocated old-space replacement plus the old->new address map.
/// Construction validates every occupied object extent before copying and
/// performs no mutation of the live machine.
pub(crate) struct OldSpaceCompaction {
    arenas: Vec<OldArena>,
    forwarding: HashMap<usize, usize>,
    reachable_external_storage: HashMap<usize, ExternalStorageKind>,
    source_pointer_slots: Vec<usize>,
    pointer_slots: Vec<usize>,
    before_bytes: usize,
    after_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OldSpaceCompactionStats {
    pub before_bytes: usize,
    pub after_bytes: usize,
    pub reclaimed_bytes: usize,
    pub live_objects: usize,
}

impl OldSpaceCompaction {
    pub(crate) fn relocated(&self, pointer: *mut u8) -> Option<*mut u8> {
        self.forwarding
            .get(&(pointer as usize))
            .copied()
            .map(|p| p as *mut u8)
    }

    pub(crate) fn contains_old(&self, pointer: *mut u8) -> bool {
        self.forwarding.contains_key(&(pointer as usize))
    }

    /// Payload allocations named by reachable pointer-carrying Lit wrappers.
    /// The external-storage owner validates and traces these allocations before
    /// commit; this plan never dereferences or frees them.
    pub(crate) fn reachable_external_storage(
        &self,
    ) -> impl Iterator<Item = (*mut u8, ExternalStorageKind)> + '_ {
        self.reachable_external_storage
            .iter()
            .map(|(&address, &kind)| (address as *mut u8, kind))
    }

    /// Pointer-bearing fields in reachable objects in the current old-space.
    /// These remain valid until commit and let the major collector discover
    /// nursery objects reached only through a live old owner. Slots belonging
    /// to dead old objects are deliberately absent.
    pub(crate) fn source_pointer_slots(&self) -> impl Iterator<Item = *mut *mut u8> + '_ {
        self.source_pointer_slots
            .iter()
            .copied()
            .map(|address| address as *mut *mut u8)
    }
}

/// Stable slots and external payloads found while validating one packed heap
/// region. The major collector uses this for the live nursery after first
/// running an ordinary complete-snapshot minor collection.
pub(crate) struct HeapRegionReachability {
    pub(crate) pointer_slots: Vec<*mut *mut u8>,
    pub(crate) external_storage: HashMap<*mut u8, ExternalStorageKind>,
}

/// Append-only, growable gen-1 region for tenured bindings.
///
/// ## Address stability
///
/// Backed by an **arena chain**: every `OldArena` owns fixed `u64` storage that
/// is allocated once and never reallocated. Only the outer Vec's metadata can
/// move when the chain grows; that does not move an arena's object bytes.
/// `cursor` is a bump offset into the last arena.
///
/// A single `Vec<u8>` backing store is FORBIDDEN — reallocation would move all
/// already-tenured objects, invalidating every live [`RootSlot`].
///
/// ## Root slot stability
///
/// Each `tenure` call allocates a `Box<*mut u8>` heap cell to hold the tenured
/// root pointer, stores the Box in `slots`, and registers the cell's address
/// as a persistent GC root. When `slots` reallocates its backing array the Box
/// VALUES (fat pointers) move, but the heap allocations they point to do not.
pub struct OldSpace {
    /// Arena chain — inner arenas never reallocate; byte addresses are stable.
    arenas: Vec<OldArena>,
    /// Bump offset into the last arena.
    cursor: usize,
    /// Total bytes tenured across all arenas.
    used: usize,
    /// Stable heap cells holding each tenured root pointer (GC slot addresses).
    /// Freed in OldSpace::drop — must happen AFTER `free_session_heap` clears
    /// PERSISTENT_ROOTS (the correct ordering when OldSpace is a field of the
    /// JitEffectMachine and free_session_heap runs in Drop::drop before fields).
    ///
    /// The `Box` is LOAD-BEARING, not redundant (clippy::vec_box fires here):
    /// the registered persistent root is the address of the *inner cell*. A bare
    /// `Vec<*mut u8>` would relocate that address on reallocation, dangling every
    /// already-registered `RootSlot`. The Box pins each cell's address for life.
    #[allow(clippy::vec_box)]
    slots: Vec<Box<*mut u8>>,
}

// SAFETY: OldSpace is used exclusively from the session-resident JIT thread
// that also drives the GC. Raw pointers are only valid on that thread.
unsafe impl Send for OldSpace {}

impl Default for OldSpace {
    fn default() -> Self {
        Self::new()
    }
}

impl OldSpace {
    /// Create an empty old-space.
    pub fn new() -> Self {
        OldSpace {
            arenas: Vec::new(),
            cursor: 0,
            used: 0,
            slots: Vec::new(),
        }
    }

    /// Tenure the value graph rooted at `ptr` from the nursery into old-space.
    ///
    /// Evacuates `ptr`'s *entire* transitive closure at tenure time, returns a
    /// [`RootSlot`] holding the tenured root pointer, and registers that slot
    /// as a persistent GC root ([`crate::host_fns::register_persistent_root`])
    /// so minor GCs keep it live and never strand it. Later mutations, including
    /// thunk memoization, record their edges through the write barrier at the
    /// store itself. Also arms the write barrier (idempotent) and
    /// registers every arena this call grows into with `MachineState`, for a
    /// diagnostic pass that needs old-space bounds reachable from vmctx alone.
    ///
    /// `nursery_from` is the nursery range to evacuate out of (typically
    /// `MachineState::gc_active_range`); objects outside it are already
    /// stable (old-space, static, poison) and are left untouched.
    ///
    /// Idempotent per object via forwarding pointers, exactly like
    /// `raw::cheney_copy`.
    ///
    /// # Safety
    /// `ptr` must be a valid heap object; `nursery_from` must bound the live
    /// nursery; `vmctx` must be the live `VMContext` for the run this tenure
    /// happens during (its `machine_state` is where the persistent root and
    /// write-barrier registrations land — see `host_fns::register_persistent_root`
    /// / `host_fns::write_barrier`); the returned slot is registered as a
    /// persistent root and must outlive every fragment compiled against it
    /// (machine lifetime).
    pub unsafe fn tenure(
        &mut self,
        vmctx: *mut VMContext,
        ptr: *mut u8,
        nursery_from: (*const u8, *const u8),
    ) -> RootSlot {
        // Before the first tenure there is no old-space, so no old-to-young
        // store is possible yet; arm unconditionally (idempotent, cheap even
        // when already armed) so every barrier call site after this point is live.
        crate::host_fns::arm_write_barrier(vmctx);

        let (from_start, from_end) = nursery_from;

        let needed = measure_closure_bytes(ptr, from_start, from_end);

        let mut root = ptr;

        if needed == 0 && read_tag(ptr) == TAG_FORWARDED {
            // `ptr` itself was already tenured earlier in this call (or a
            // prior tenure of overlapping structure this generation) —
            // `measure_closure_bytes` returns 0 for a forwarded object
            // without inspecting where it points, so `root = ptr` would root
            // the stale TAG_FORWARDED stub instead of the tenured copy.
            // Follow the forwarding pointer (written by `raw::evacuate` at
            // offset 8, mirroring its own forwarded-object short-circuit) so
            // a second tenure of an already-tenured object is idempotent —
            // this is what makes tenuring exactly one aliased field (the new
            // `it`-tuple bind primitive) safe even when a caller ends up
            // tenuring the same root twice (`run_multi_bind`'s
            // `(a, b) <- pure (dup, dup)`).
            root = *(ptr.add(8) as *const *mut u8);
        }

        if needed > 0 {
            // Grow arena chain if the last arena lacks contiguous free bytes.
            let free = self.arenas.last().map_or(0, |arena| {
                arena.capacity_bytes().saturating_sub(self.cursor)
            });
            if free < needed {
                self.arenas.push(OldArena::new(DEFAULT_ARENA.max(needed)));
                self.cursor = 0;
                #[allow(
                    clippy::unwrap_used,
                    reason = "an arena was just pushed on this branch, or old_space holds at least one arena by construction"
                )]
                let new_arena = self.arenas.last().unwrap();
                let arena_start = new_arena.start();
                let arena_end = arena_start.add(new_arena.capacity_bytes());
                crate::host_fns::register_old_space_arena(vmctx, arena_start, arena_end);
            }

            // Copy the closure into the current arena via Cheney's algorithm.
            // to_slice is wholly within a stable inner-arena allocation.
            #[allow(
                clippy::unwrap_used,
                reason = "an arena was just pushed on this branch, or old_space holds at least one arena by construction"
            )]
            let arena = self.arenas.last_mut().unwrap();
            let to_slice = &mut arena.bytes_mut()[self.cursor..self.cursor + needed];
            let copied_start = to_slice.as_mut_ptr();

            let res = cheney_copy(&[&mut root as *mut *mut u8], from_start, from_end, to_slice);

            debug_assert_eq!(
                res.bytes_copied, needed,
                "tenure: measure_closure_bytes({needed}) ≠ cheney_copy bytes_copied({})",
                res.bytes_copied
            );

            self.cursor += res.bytes_copied;
            arena.used = self.cursor;
            self.used += res.bytes_copied;

            // A boxed payload can be initialized before old-space exists,
            // while the barrier is deliberately disarmed. Once its wrapper is
            // tenured, register those stable payload slots before the fixup
            // minor collection so their nursery children participate.
            if !vmctx.is_null() {
                let external_edges = unsafe { scan_heap_region(copied_start, res.bytes_copied) }
                    .and_then(|reach| {
                        let ms = unsafe { crate::machine_state::machine_state(vmctx) };
                        ms.remember_external_payload_edges(reach.external_storage)
                            .map_err(|error| format!("invalid tenured external payload: {error:?}"))
                    });
                if external_edges.is_err() {
                    unsafe { crate::machine_state::machine_state(vmctx) }
                        .set_first_cause(crate::host_fns::RuntimeError::BadPointer);
                }
            }

            // Sibling-reference fixup (see the module doc's "The write
            // barrier" section, and `run_minor_collection_for_tenure_fixup`'s
            // own doc): the copy above only visited THIS tenure root's own
            // transitive graph. Any other live nursery object that
            // independently holds a pointer into what was just moved is left
            // pointing at the pre-tenure address, which now reads as a
            // TAG_FORWARDED stub. A real minor collection over every ordinary
            // root category, run immediately here, fixes every such sibling
            // via the same forward-following logic already proven correct
            // for shared substructure (`test_overlapping_tenures_preserve_sharing`
            // below). Gated on `needed > 0`: nothing new was evacuated this
            // call in the `needed == 0` branches, so there is nothing new to
            // fix up (whatever tenure call actually moved the shared object
            // already ran this fixup itself).
            crate::host_fns::run_minor_collection_for_tenure_fixup(vmctx);
        }

        // Allocate a stable heap cell to hold the root pointer.
        // Vec<Box<_>> reallocs move Box values but not their heap allocations,
        // so `slot` (the allocation address) is stable across future pushes.
        let mut b = Box::new(root);
        let slot: *mut *mut u8 = &mut *b;
        self.slots.push(b);

        // Register as a persistent GC root so future minor (and major) GCs can
        // update *slot in-place if the tenured object ever relocates.
        crate::host_fns::register_persistent_root(vmctx, slot);

        RootSlot::new(slot)
    }

    /// Total bytes currently tenured (test/diagnostic accessor).
    pub fn bytes_used(&self) -> usize {
        self.used
    }

    pub(crate) fn contains(&self, pointer: *const u8) -> bool {
        let address = pointer as usize;
        self.arenas.iter().any(|arena| {
            let start = arena.start() as usize;
            address >= start && address < start + arena.used
        })
    }

    fn contains_allocation(&self, pointer: *const u8) -> bool {
        let address = pointer as usize;
        self.arenas.iter().any(|arena| {
            let start = arena.start() as usize;
            address >= start && address < start + arena.capacity_bytes()
        })
    }

    pub(crate) fn occupied_ranges(&self) -> Vec<(*const u8, *const u8)> {
        self.arenas
            .iter()
            .filter(|arena| arena.used != 0)
            .map(|arena| {
                let start = arena.start();
                // SAFETY: used never exceeds this arena's allocation.
                (start, unsafe { start.add(arena.used) })
            })
            .collect()
    }

    /// Trace old-space transitively from `root_slots`, validate every occupied
    /// object and reachable edge, and stage a packed replacement graph.
    /// Internal fields in the staged graph are rewritten before this returns,
    /// so committing can never expose an edge into a retired arena.
    ///
    /// Pointer-carrying Lit wrappers contribute their payload identities to the
    /// returned plan. The external-storage owner must validate those payloads
    /// and, for boxed arrays, include their element slots in a subsequent trace
    /// before committing the final plan.
    ///
    /// # Safety
    /// Every root slot must be non-null and valid to read for the duration of
    /// the call.
    pub(crate) unsafe fn stage_compaction(
        &self,
        root_slots: &[*mut *mut u8],
    ) -> Result<OldSpaceCompaction, String> {
        let mut objects = HashMap::<usize, usize>::new();
        let mut object_order = Vec::new();
        for arena in &self.arenas {
            let mut offset = 0usize;
            while offset < arena.used {
                let pointer = unsafe { arena.start().add(offset) as *mut u8 };
                let size = unsafe { read_size(pointer) as usize };
                if size < 8 {
                    return Err(format!("old-space object at {pointer:p} has size {size}"));
                }
                let aligned = size
                    .checked_add(7)
                    .map(|n| n & !7)
                    .ok_or_else(|| format!("old-space object at {pointer:p} size overflows"))?;
                if offset + aligned > arena.used {
                    return Err(format!(
                        "old-space object at {pointer:p} extends past occupied arena"
                    ));
                }
                validate_object_shape(pointer, size)?;
                objects.insert(pointer as usize, aligned);
                object_order.push(pointer as usize);
                offset += aligned;
            }
        }

        let mut work = VecDeque::new();
        let mut live = HashSet::<usize>::new();
        let mut source_pointer_slots = Vec::new();
        for &slot in root_slots {
            if slot.is_null() {
                return Err("null old-space root slot".into());
            }
            enqueue_old_pointer(self, &objects, unsafe { *slot }, "root", &mut work)?;
        }

        let mut reachable_external_storage = HashMap::new();
        while let Some(address) = work.pop_front() {
            if !live.insert(address) {
                continue;
            }
            let pointer = address as *mut u8;
            let size = unsafe { read_size(pointer) as usize };
            for offset in unsafe { validated_pointer_offsets(pointer, size)? } {
                let slot = unsafe { pointer.add(offset) as *mut *mut u8 };
                source_pointer_slots.push(slot as usize);
                let child = unsafe { *slot };
                enqueue_old_pointer(self, &objects, child, "object field", &mut work)?;
            }
            if unsafe { read_tag(pointer) } == HeapTag::Lit.as_byte() {
                let lit_tag = LitTag::from_byte(unsafe { *pointer.add(LIT_TAG_OFFSET) })
                    .ok_or_else(|| format!("old-space Lit at {pointer:p} has invalid tag"))?;
                if matches!(
                    lit_tag,
                    LitTag::String | LitTag::ByteArray | LitTag::SmallArray | LitTag::Array
                ) {
                    let payload = unsafe { *(pointer.add(LIT_VALUE_OFFSET) as *const *mut u8) };
                    if !payload.is_null() {
                        insert_external_storage(
                            &mut reachable_external_storage,
                            payload as usize,
                            external_storage_kind(lit_tag).ok_or_else(|| {
                                format!("old-space Lit at {pointer:p} lacks an external-storage kind")
                            })?,
                        )?;
                    }
                }
            }
        }

        let after_bytes = object_order
            .iter()
            .filter(|address| live.contains(address))
            .try_fold(0usize, |sum, address| sum.checked_add(objects[address]))
            .ok_or_else(|| "live old-space size overflow".to_string())?;
        let mut arenas = if after_bytes == 0 {
            Vec::new()
        } else {
            vec![OldArena::new(DEFAULT_ARENA.max(after_bytes))]
        };
        let mut forwarding = HashMap::new();
        let mut pointer_slots = Vec::new();
        if let Some(destination) = arenas.first_mut() {
            let mut cursor = 0usize;
            for &address in &object_order {
                if live.contains(&address) {
                    let old = address as *mut u8;
                    let size = objects[&address];
                    let new = unsafe { destination.start_mut().add(cursor) };
                    unsafe { std::ptr::copy_nonoverlapping(old, new, size) };
                    forwarding.insert(address, new as usize);
                    cursor += size;
                }
            }
            destination.used = cursor;

            // Repair staged bytes, not the retiring source graph. Sharing and
            // cycles are preserved because the complete address map exists
            // before any field is visited.
            for &address in &object_order {
                let Some(&new_address) = forwarding.get(&address) else {
                    continue;
                };
                let new = new_address as *mut u8;
                let size = unsafe { read_size(new) as usize };
                for offset in unsafe { validated_pointer_offsets(new, size)? } {
                    let field = unsafe { new.add(offset) as *mut *mut u8 };
                    pointer_slots.push(field as usize);
                    let child = unsafe { *field };
                    if self.contains(child) {
                        let relocated = forwarding.get(&(child as usize)).ok_or_else(|| {
                            format!(
                                "reachable object {new:p} points to unstaged old object {child:p}"
                            )
                        })?;
                        unsafe { *field = *relocated as *mut u8 };
                    }
                }
            }
        }

        Ok(OldSpaceCompaction {
            arenas,
            forwarding,
            reachable_external_storage,
            source_pointer_slots,
            pointer_slots,
            before_bytes: self.used,
            after_bytes,
        })
    }

    /// Rewrite every supplied slot, then atomically replace old-space arenas.
    /// All fallible validation must happen before this commit method.
    ///
    /// # Safety
    /// Each slot must remain valid through the call and either contain null, a
    /// pointer outside old-space, or a live old-space object represented by
    /// `plan`.
    pub(crate) unsafe fn commit_compaction(
        &mut self,
        machine_state: &crate::machine_state::MachineState,
        plan: OldSpaceCompaction,
        rewrite_slots: &[*mut *mut u8],
        nursery: (*const u8, *const u8),
    ) -> Result<OldSpaceCompactionStats, String> {
        let remembered_slots: Vec<_> = plan
            .pointer_slots
            .iter()
            .copied()
            .map(|address| address as *mut *mut u8)
            .filter(|&slot| {
                let value = unsafe { *slot } as usize;
                value >= nursery.0 as usize && value < nursery.1 as usize
            })
            .collect();

        for &slot in rewrite_slots {
            if slot.is_null() {
                return Err("null old-space rewrite slot".into());
            }
            let pointer = unsafe { *slot };
            if self.contains(pointer) && !plan.contains_old(pointer) {
                return Err(format!(
                    "slot {slot:p} points to dead old-space object {pointer:p}"
                ));
            }
        }

        machine_state.clear_remembered_slots();
        for &slot in rewrite_slots {
            let pointer = unsafe { *slot };
            if let Some(relocated) = plan.relocated(pointer) {
                unsafe { *slot = relocated };
            }
        }

        for arena in &self.arenas {
            let start = arena.start();
            let end = unsafe { start.add(arena.capacity_bytes()) };
            machine_state.retire_old_space_arena(start, end);
        }
        for arena in &plan.arenas {
            let start = arena.start();
            let end = unsafe { start.add(arena.capacity_bytes()) };
            machine_state.register_old_space_arena(start, end);
        }
        for slot in remembered_slots {
            machine_state.register_remembered_slot(slot);
        }

        let stats = OldSpaceCompactionStats {
            before_bytes: plan.before_bytes,
            after_bytes: plan.after_bytes,
            reclaimed_bytes: plan.before_bytes.saturating_sub(plan.after_bytes),
            live_objects: plan.forwarding.len(),
        };
        self.arenas = plan.arenas;
        self.cursor = self.arenas.last().map_or(0, |arena| arena.used);
        self.used = plan.after_bytes;
        Ok(stats)
    }
}

/// Validate and enumerate every object in a packed moving-heap prefix.
///
/// # Safety
/// `start..start+used` must be readable initialized heap storage whose objects
/// occupy the prefix exactly.
pub(crate) unsafe fn scan_heap_region(
    start: *mut u8,
    used: usize,
) -> Result<HeapRegionReachability, String> {
    let mut pointer_slots = Vec::new();
    let mut external_storage = HashMap::new();
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
        let raw_tag = unsafe { read_tag(pointer) };
        if raw_tag != TAG_FORWARDED {
            for field_offset in unsafe { validated_pointer_offsets(pointer, size)? } {
                pointer_slots.push(unsafe { pointer.add(field_offset) as *mut *mut u8 });
            }
        }
        if raw_tag == HeapTag::Lit.as_byte() {
            let tag = LitTag::from_byte(unsafe { *pointer.add(LIT_TAG_OFFSET) })
                .ok_or_else(|| format!("Lit at {pointer:p} has invalid tag"))?;
            if let Some(kind) = external_storage_kind(tag) {
                let payload = unsafe { *(pointer.add(LIT_VALUE_OFFSET) as *const *mut u8) };
                if !payload.is_null() {
                    insert_external_storage(&mut external_storage, payload as usize, kind)?;
                }
            }
        }
        offset += aligned;
    }
    Ok(HeapRegionReachability {
        pointer_slots,
        external_storage: external_storage
            .into_iter()
            .map(|(address, kind)| (address as *mut u8, kind))
            .collect(),
    })
}

/// Validate a packed region, then enumerate only objects reachable from the
/// supplied root slots. Pointers outside the region are left to the old/static
/// owners; an interior pointer into this region is rejected.
///
/// # Safety
/// The region contract matches [`scan_heap_region`], and every root slot must
/// be valid to read for the duration of this call.
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
    let mut pointer_slots = Vec::new();
    let mut external_storage = HashMap::new();
    while let Some(address) = work.pop_front() {
        if !live.insert(address) {
            continue;
        }
        let pointer = address as *mut u8;
        let size = objects[&address];
        for field_offset in unsafe { validated_pointer_offsets(pointer, size)? } {
            let slot = unsafe { pointer.add(field_offset) as *mut *mut u8 };
            pointer_slots.push(slot);
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
        pointer_slots,
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

fn enqueue_old_pointer(
    old_space: &OldSpace,
    objects: &HashMap<usize, usize>,
    pointer: *mut u8,
    source: &str,
    work: &mut VecDeque<usize>,
) -> Result<(), String> {
    if pointer.is_null() || !old_space.contains_allocation(pointer) {
        return Ok(());
    }
    if !old_space.contains(pointer) || !objects.contains_key(&(pointer as usize)) {
        return Err(format!(
            "{source} points to non-object old-space address {pointer:p}"
        ));
    }
    work.push_back(pointer as usize);
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
            if size < THUNK_MIN_SIZE || (size - THUNK_CAPTURED_OFFSET) % FIELD_STRIDE != 0 {
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

/// Compute the total aligned bytes occupied by all heap objects in `ptr`'s
/// transitive closure that fall within `[from_start, from_end)`.
///
/// Uses an explicit work-stack (no host recursion — graphs can be arbitrarily
/// deep) and a visited set to count each object exactly once. The alignment
/// formula `(size + 7) & !7` matches Cheney's, so the result equals the number
/// of bytes `cheney_copy` will write for the same root.
unsafe fn measure_closure_bytes(ptr: *mut u8, from_start: *const u8, from_end: *const u8) -> usize {
    let in_range = |p: *const u8| -> bool {
        (p as usize) >= (from_start as usize) && (p as usize) < (from_end as usize)
    };

    if !in_range(ptr as *const u8) {
        return 0;
    }

    let mut visited: HashSet<*mut u8> = HashSet::new();
    let mut work: Vec<*mut u8> = vec![ptr];
    let mut total: usize = 0;

    while let Some(obj) = work.pop() {
        if !visited.insert(obj) {
            continue;
        }
        // An already-forwarded object (a prior tenure of an overlapping graph,
        // within the same nursery generation) is copied as ZERO bytes by
        // `cheney_copy` — it returns the existing forward target and does not
        // re-scan. Skip it here so `measure` matches `bytes_copied`, honoring
        // the per-object idempotency the `tenure` doc promises. Without this,
        // a second tenure that shares substructure over-counts and trips the
        // `measure == bytes_copied` invariant whenever a bind path tenures
        // multiple values sharing substructure within one generation.
        if read_tag(obj) == TAG_FORWARDED {
            continue;
        }
        let size = read_size(obj) as usize;
        let aligned = (size + 7) & !7;
        total += aligned;

        // Real bytes readable from obj: from-range containment (checked via
        // `in_range` above and at push-time below) guarantees obj < from_end.
        let avail = from_end as usize - obj as usize;
        for_each_pointer_field(obj, avail, |field_slot| {
            let field_val = *field_slot;
            if !field_val.is_null()
                && in_range(field_val as *const u8)
                && !visited.contains(&field_val)
            {
                work.push(field_val);
            }
        });
    }

    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tidepool_heap::gc::raw::cheney_copy;
    use tidepool_heap::layout::*;

    #[repr(align(8))]
    struct AlignedBuf([u8; 4096]);

    unsafe fn write_lit(buf: &mut [u8], offset: usize, value: i64) -> usize {
        let ptr = buf.as_mut_ptr().add(offset);
        write_header(ptr, TAG_LIT, LIT_SIZE as u32);
        *ptr.add(LIT_TAG_OFFSET) = LitTag::Int as u8;
        *(ptr.add(LIT_VALUE_OFFSET) as *mut i64) = value;
        offset + LIT_SIZE
    }

    unsafe fn write_con(buf: &mut [u8], offset: usize, con_tag: u64, fields: &[*mut u8]) -> usize {
        let ptr = buf.as_mut_ptr().add(offset);
        let size = (CON_FIELDS_OFFSET + fields.len() * FIELD_STRIDE) as u32;
        let aligned = ((size as usize) + 7) & !7;
        write_header(ptr, TAG_CON, size);
        *(ptr.add(CON_TAG_OFFSET) as *mut u64) = con_tag;
        *(ptr.add(CON_NUM_FIELDS_OFFSET) as *mut u16) = fields.len() as u16;
        for (i, &f) in fields.iter().enumerate() {
            *(ptr.add(CON_FIELDS_OFFSET + i * FIELD_STRIDE) as *mut *mut u8) = f;
        }
        offset + aligned
    }

    /// Test (a): a tenured value's heap address is stable across N>=3 simulated
    /// minor GCs. The minor GC from-range is a fresh nursery each round (never
    /// old-space), so Cheney's `is_in_range` skips the tenured pointer and
    /// leaves `*slot` unchanged.
    #[test]
    #[serial]
    fn test_tenured_survives_minor_gcs() {
        unsafe {
            // Build Con(tag=7, fields=[Lit(42)]) in the nursery buffer.
            let mut nursery = AlignedBuf([0u8; 4096]);
            let n = &mut nursery.0;
            let lit_off = write_lit(n, 0, 42);
            let lit_ptr = n.as_mut_ptr();
            let _con_end = write_con(n, lit_off, 7, &[lit_ptr]);
            let con_ptr = n.as_mut_ptr().add(lit_off);

            let from_start = n.as_ptr();
            let from_end = n.as_ptr().add(n.len());

            let mut old_space = OldSpace::new();
            let slot = old_space.tenure(std::ptr::null_mut(), con_ptr, (from_start, from_end));

            // Record the tenured address and verify initial content.
            let a: *mut u8 = slot.current();
            assert_eq!(read_tag(a), TAG_CON, "tenured object should be a Con");
            assert_eq!(*(a.add(CON_TAG_OFFSET) as *const u64), 7);
            let f0 = *(a.add(CON_FIELDS_OFFSET) as *const *mut u8);
            assert_eq!(read_tag(f0), TAG_LIT);
            assert_eq!(*(f0.add(LIT_VALUE_OFFSET) as *const i64), 42);

            // Simulate N=3 minor GCs, each over a fresh nursery buffer.
            // slot.addr() is included in the root set to mirror how perform_gc
            // would include PERSISTENT_ROOTS; because *slot == a is outside
            // the fresh nursery's from-range, Cheney does not update it.
            for round in 0u32..3 {
                let mut fresh = AlignedBuf([0u8; 4096]);
                let fn_ = &mut fresh.0;
                // Some nursery-resident content that DOES move.
                write_lit(fn_, 0, round as i64);

                let fresh_start = fn_.as_ptr();
                let fresh_end = fn_.as_ptr().add(fn_.len());
                let mut ts = vec![0u8; fn_.len()];

                cheney_copy(&[slot.addr()], fresh_start, fresh_end, &mut ts);

                assert_eq!(
                    slot.current(),
                    a,
                    "tenured address changed after minor GC round {round}"
                );

                // Object content must be intact after each GC.
                assert_eq!(read_tag(a), TAG_CON);
                assert_eq!(*(a.add(CON_TAG_OFFSET) as *const u64), 7);
                let f = *(a.add(CON_FIELDS_OFFSET) as *const *mut u8);
                assert_eq!(read_tag(f), TAG_LIT);
                assert_eq!(*(f.add(LIT_VALUE_OFFSET) as *const i64), 42);
            }
        }
    }

    /// Test (b): minor-GC byte cost is independent of old_space occupancy.
    ///
    /// Old-space lives outside the nursery from-range, so Cheney's `is_in_range`
    /// never scans it. The number of bytes copied from a fixed nursery live-set
    /// must be identical whether 1 or 100 objects have been tenured.
    #[test]
    #[serial]
    fn test_minor_gc_cost_independent_of_old_space_size() {
        // Fixed live-set: Lit(100) + Con(tag=5, fields=[Lit]).
        // Con size = CON_FIELDS_OFFSET + 1*FIELD_STRIDE = 24+8 = 32 (already aligned).
        // Expected bytes_copied = LIT_SIZE + 32 = 24 + 32 = 56.
        unsafe fn fixed_nursery_gc_bytes() -> usize {
            let mut buf = AlignedBuf([0u8; 4096]);
            let n = &mut buf.0;
            let lit_off = write_lit(n, 0, 100);
            let lit_ptr = n.as_mut_ptr();
            let _end = write_con(n, lit_off, 5, &[lit_ptr]);
            let con_ptr = n.as_mut_ptr().add(lit_off);

            let mut root = con_ptr;
            let from_s = n.as_ptr();
            let from_e = n.as_ptr().add(n.len());
            let mut ts = vec![0u8; n.len()];
            cheney_copy(&[&mut root as *mut *mut u8], from_s, from_e, &mut ts).bytes_copied
        }

        unsafe {
            // ── Scenario 1: 1 tenured object ─────────────────────────────────
            let bytes_1 = {
                let mut extra = AlignedBuf([0u8; 4096]);
                let e = &mut extra.0;
                write_lit(e, 0, 1);
                let ptr = e.as_mut_ptr();
                let extra_range = (e.as_ptr(), e.as_ptr().add(e.len()));
                let mut os = OldSpace::new();
                os.tenure(std::ptr::null_mut(), ptr, extra_range);
                // os lives across the GC call; this test supplies its own
                // explicit root set (fixed_nursery_gc_bytes), independent of
                // persistent-root registration.
                fixed_nursery_gc_bytes()
            };

            // ── Scenario 2: 100 tenured objects ──────────────────────────────
            // 100 * LIT_SIZE = 100 * 24 = 2400 bytes, fits in 4096.
            let bytes_2 = {
                let mut extra = AlignedBuf([0u8; 4096]);
                let e = &mut extra.0;
                let extra_range = (e.as_ptr(), e.as_ptr().add(e.len()));
                let mut os = OldSpace::new();
                for i in 0i64..100 {
                    let off = (i as usize) * LIT_SIZE;
                    write_lit(e, off, i);
                    // Each Lit is independent; no pointer fields, so earlier
                    // forwarding pointers don't interfere with later measures.
                    let ptr = e.as_mut_ptr().add(off);
                    os.tenure(std::ptr::null_mut(), ptr, extra_range);
                }
                // os and its 100 slots live across the GC call.
                fixed_nursery_gc_bytes()
            };

            assert_eq!(
                bytes_1, bytes_2,
                "minor GC must not scan old_space: \
                 bytes_1={bytes_1}, bytes_2={bytes_2}"
            );
        }
    }

    /// Regression: tenuring two graphs that SHARE substructure within the same
    /// nursery generation. The first tenure forwards the shared node; the
    /// second tenure's `measure_closure_bytes` must skip that forwarded node to
    /// match `cheney_copy`'s copy-once `bytes_copied` (else the
    /// `measure == bytes_copied` debug_assert in `tenure` fires). Also proves
    /// sharing is preserved: both tenured roots point at the SAME old-space copy
    /// of the shared child.
    #[test]
    #[serial]
    fn test_overlapping_tenures_preserve_sharing() {
        unsafe {
            // Nursery: Lit(99) shared by Con A(tag=1,[Lit]) and Con B(tag=2,[Lit]).
            let mut nursery = AlignedBuf([0u8; 4096]);
            let n = &mut nursery.0;
            let a_off = write_lit(n, 0, 99);
            let lit_ptr = n.as_mut_ptr();
            let b_off = write_con(n, a_off, 1, &[lit_ptr]);
            let a_ptr = n.as_mut_ptr().add(a_off);
            let _end = write_con(n, b_off, 2, &[lit_ptr]);
            let b_ptr = n.as_mut_ptr().add(b_off);

            let range = (n.as_ptr(), n.as_ptr().add(n.len()));

            let mut old_space = OldSpace::new();

            // First tenure forwards both A and the shared Lit in the nursery.
            let slot_a = old_space.tenure(std::ptr::null_mut(), a_ptr, range);
            let used_after_a = old_space.bytes_used();
            // Con(32, aligned) + Lit(24) = 56.
            assert_eq!(used_after_a, 56, "A + shared Lit");

            // Second tenure: B is fresh, its child Lit is already forwarded.
            // Without the forwarded-skip in measure, this panics on the
            // measure!=bytes_copied debug_assert.
            let slot_b = old_space.tenure(std::ptr::null_mut(), b_ptr, range);
            // Only B's own 32 bytes are newly copied (Lit already in old-space).
            assert_eq!(old_space.bytes_used(), 56 + 32, "B only; Lit not recopied");

            // Sharing preserved: A and B point at the SAME old-space Lit.
            let a_copy = slot_a.current();
            let b_copy = slot_b.current();
            assert_eq!(*(a_copy.add(CON_TAG_OFFSET) as *const u64), 1);
            assert_eq!(*(b_copy.add(CON_TAG_OFFSET) as *const u64), 2);
            let a_child = *(a_copy.add(CON_FIELDS_OFFSET) as *const *mut u8);
            let b_child = *(b_copy.add(CON_FIELDS_OFFSET) as *const *mut u8);
            assert_eq!(a_child, b_child, "shared child must be one old-space copy");
            assert_eq!(read_tag(a_child), TAG_LIT);
            assert_eq!(*(a_child.add(LIT_VALUE_OFFSET) as *const i64), 99);
        }
    }

    /// Tenuring the SAME top-level root pointer twice in one call (e.g.
    /// `(a, b) <- pure (dup, dup)`, where both tuple fields are the same heap
    /// object) must follow the forwarding pointer on the second tenure
    /// rather than rooting the stale `TAG_FORWARDED` stub directly — the
    /// latter corrupts `run_multi_bind`, resolving to `heap tag: 255` instead
    /// of the tenured value. Unlike `test_overlapping_tenures_preserve_sharing`
    /// (a shared CHILD reached through a field, already handled by
    /// `cheney_copy`/`evacuate`'s own forwarding check), here the ROOT of the
    /// second `tenure()` call is itself already forwarded by the first.
    #[test]
    #[serial]
    fn test_tenure_same_root_twice_follows_forward() {
        unsafe {
            let mut nursery = AlignedBuf([0u8; 4096]);
            let n = &mut nursery.0;
            write_lit(n, 0, 42);
            let dup_ptr = n.as_mut_ptr();

            let range = (n.as_ptr(), n.as_ptr().add(n.len()));
            let mut old_space = OldSpace::new();

            // First tenure of dup_ptr: fresh, copies normally and forwards
            // the nursery original.
            let slot_a = old_space.tenure(std::ptr::null_mut(), dup_ptr, range);
            let used_after_first = old_space.bytes_used();
            assert_eq!(used_after_first, LIT_SIZE);

            // Second tenure of the SAME top-level pointer — dup_ptr's header
            // is now TAG_FORWARDED from the first tenure.
            let slot_b = old_space.tenure(std::ptr::null_mut(), dup_ptr, range);
            assert_eq!(
                old_space.bytes_used(),
                used_after_first,
                "shared root must not be recopied"
            );

            let a_copy = slot_a.current();
            let b_copy = slot_b.current();
            assert_eq!(
                a_copy, b_copy,
                "both tenures of the same root must resolve to one old-space copy"
            );
            assert_eq!(
                read_tag(b_copy),
                TAG_LIT,
                "tenuring an already-forwarded root must follow the forward pointer, \
                 not root the stale TAG_FORWARDED stub"
            );
            assert_eq!(*(b_copy.add(LIT_VALUE_OFFSET) as *const i64), 42);
        }
    }

    #[test]
    #[serial]
    fn trace_region_normalizes_exact_forwarded_child_but_rejects_interior_pointer() {
        unsafe {
            let mut nursery = AlignedBuf([0u8; 4096]);
            let bytes = &mut nursery.0;
            let forwarded_offset = write_con(bytes, 0, 1, &[std::ptr::null_mut()]);
            let end = write_lit(bytes, forwarded_offset, 7);
            let owner = bytes.as_mut_ptr();
            let forwarded = bytes.as_mut_ptr().add(forwarded_offset);
            *(owner.add(CON_FIELDS_OFFSET) as *mut *mut u8) = forwarded;

            let mut stable = AlignedBuf([0u8; 4096]);
            write_lit(&mut stable.0, 0, 7);
            let stable_target = stable.0.as_mut_ptr();
            *forwarded = TAG_FORWARDED;
            *(forwarded.add(8) as *mut *mut u8) = stable_target;

            let mut root = owner;
            let reach = trace_heap_region(bytes.as_mut_ptr(), end, &[&mut root as *mut *mut u8])
                .expect("exact forwarded child should normalize");
            assert_eq!(reach.pointer_slots.len(), 1);
            assert_eq!(reach.pointer_slots[0], owner.add(CON_FIELDS_OFFSET).cast());
            assert_eq!(
                *(owner.add(CON_FIELDS_OFFSET) as *const *mut u8),
                stable_target,
                "the owning field must no longer retain a nursery forwarding stub"
            );

            let mut interior = owner.add(8);
            let Err(error) =
                trace_heap_region(bytes.as_mut_ptr(), end, &[&mut interior as *mut *mut u8])
            else {
                panic!("an interior nursery pointer is not a forwarding stub");
            };
            assert!(error.contains("non-object nursery address"));
        }
    }

    #[test]
    #[serial]
    fn major_compaction_rewrites_live_slot_and_reclaims_dead_object() {
        unsafe {
            let mut nursery = AlignedBuf([0u8; 4096]);
            let bytes = &mut nursery.0;
            write_lit(bytes, 0, 11);
            write_lit(bytes, LIT_SIZE, 22);
            let range = (bytes.as_ptr(), bytes.as_ptr().add(bytes.len()));
            let mut old_space = OldSpace::new();
            let live = old_space.tenure(std::ptr::null_mut(), bytes.as_mut_ptr(), range);
            let dead = old_space.tenure(
                std::ptr::null_mut(),
                bytes.as_mut_ptr().add(LIT_SIZE),
                range,
            );
            let live_slot_address = live.addr();
            let live_before = live.current();
            let dead_before = dead.current();
            assert_eq!(old_space.bytes_used(), 2 * LIT_SIZE);

            let plan = old_space
                .stage_compaction(&[live.addr()])
                .expect("trace and stage live object");
            let machine_state = crate::machine_state::MachineState::new();
            let stats = old_space
                .commit_compaction(
                    &machine_state,
                    plan,
                    &[live.addr()],
                    (std::ptr::null(), std::ptr::null()),
                )
                .expect("commit compact live object");

            assert_eq!(live.addr(), live_slot_address);
            assert_ne!(live.current(), live_before);
            assert_ne!(live.current(), dead_before);
            assert_eq!(read_tag(live.current()), TAG_LIT);
            assert_eq!(*(live.current().add(LIT_VALUE_OFFSET) as *const i64), 11);
            assert_eq!(stats.before_bytes, 2 * LIT_SIZE);
            assert_eq!(stats.after_bytes, LIT_SIZE);
            assert_eq!(stats.reclaimed_bytes, LIT_SIZE);
            assert_eq!(stats.live_objects, 1);
            assert_eq!(old_space.bytes_used(), LIT_SIZE);
        }
    }

    #[test]
    #[serial]
    fn major_compaction_rejects_interior_live_pointer_before_mutation() {
        unsafe {
            let mut nursery = AlignedBuf([0u8; 4096]);
            let bytes = &mut nursery.0;
            write_lit(bytes, 0, 11);
            let range = (bytes.as_ptr(), bytes.as_ptr().add(bytes.len()));
            let mut old_space = OldSpace::new();
            let root = old_space.tenure(std::ptr::null_mut(), bytes.as_mut_ptr(), range);
            let before = root.current();

            let mut interior = before.add(8);
            let result = old_space.stage_compaction(&[&mut interior]);
            assert!(result.is_err());
            assert_eq!(root.current(), before);
            assert_eq!(old_space.bytes_used(), LIT_SIZE);
        }
    }

    #[test]
    #[serial]
    fn major_compaction_rewrites_shared_cyclic_graph_in_staged_bytes() {
        unsafe {
            let mut nursery = AlignedBuf([0u8; 4096]);
            let bytes = &mut nursery.0;
            let a_size = write_con(bytes, 0, 1, &[std::ptr::null_mut(); 2]);
            let end = write_con(bytes, a_size, 2, &[std::ptr::null_mut()]);
            let a = bytes.as_mut_ptr();
            let b = bytes.as_mut_ptr().add(a_size);
            *(a.add(CON_FIELDS_OFFSET) as *mut *mut u8) = b;
            *(a.add(CON_FIELDS_OFFSET + FIELD_STRIDE) as *mut *mut u8) = b;
            *(b.add(CON_FIELDS_OFFSET) as *mut *mut u8) = a;

            let range = (bytes.as_ptr(), bytes.as_ptr().add(end));
            let mut old_space = OldSpace::new();
            let root = old_space.tenure(std::ptr::null_mut(), a, range);
            let old_a = root.current();
            let old_b = *(old_a.add(CON_FIELDS_OFFSET) as *mut *mut u8);

            let plan = old_space.stage_compaction(&[root.addr()]).unwrap();
            let machine_state = crate::machine_state::MachineState::new();
            let stats = old_space
                .commit_compaction(
                    &machine_state,
                    plan,
                    &[root.addr()],
                    (std::ptr::null(), std::ptr::null()),
                )
                .unwrap();

            let new_a = root.current();
            let new_b0 = *(new_a.add(CON_FIELDS_OFFSET) as *mut *mut u8);
            let new_b1 = *(new_a.add(CON_FIELDS_OFFSET + FIELD_STRIDE) as *mut *mut u8);
            assert_ne!(new_a, old_a);
            assert_ne!(new_b0, old_b);
            assert_eq!(new_b0, new_b1, "shared child must stay shared");
            assert_eq!(
                *(new_b0.add(CON_FIELDS_OFFSET) as *mut *mut u8),
                new_a,
                "cycle must point into replacement arena"
            );
            assert_eq!(stats.live_objects, 2);
        }
    }

    #[test]
    #[serial]
    fn major_compaction_reports_reachable_external_payload_identity() {
        unsafe {
            let mut nursery = AlignedBuf([0u8; 4096]);
            let bytes = &mut nursery.0;
            write_header(bytes.as_mut_ptr(), TAG_LIT, LIT_SIZE as u32);
            *bytes.as_mut_ptr().add(LIT_TAG_OFFSET) = LitTag::ByteArray as u8;
            let mut payload = vec![0u8; 16];
            *(bytes.as_mut_ptr().add(LIT_VALUE_OFFSET) as *mut *mut u8) = payload.as_mut_ptr();
            let range = (bytes.as_ptr(), bytes.as_ptr().add(LIT_SIZE));
            let mut old_space = OldSpace::new();
            let root = old_space.tenure(std::ptr::null_mut(), bytes.as_mut_ptr(), range);

            let plan = old_space.stage_compaction(&[root.addr()]).unwrap();
            assert_eq!(
                plan.reachable_external_storage().collect::<Vec<_>>(),
                vec![(payload.as_mut_ptr(), ExternalStorageKind::Bytes)]
            );
        }
    }

    #[test]
    #[serial]
    fn major_compaction_rejects_invalid_tag_and_extent_without_mutation() {
        unsafe {
            for corrupt_extent in [false, true] {
                let mut nursery = AlignedBuf([0u8; 4096]);
                let bytes = &mut nursery.0;
                write_lit(bytes, 0, 11);
                let range = (bytes.as_ptr(), bytes.as_ptr().add(LIT_SIZE));
                let mut old_space = OldSpace::new();
                let root = old_space.tenure(std::ptr::null_mut(), bytes.as_mut_ptr(), range);
                let before = root.current();
                let before_bytes = old_space.bytes_used();
                if corrupt_extent {
                    std::ptr::write_unaligned(before.add(1) as *mut u32, 4096);
                } else {
                    *before = 99;
                }

                assert!(old_space.stage_compaction(&[root.addr()]).is_err());
                assert_eq!(root.current(), before);
                assert_eq!(old_space.bytes_used(), before_bytes);
            }
        }
    }

    #[test]
    #[serial]
    fn major_compaction_failed_commit_retains_slots_and_old_arena() {
        unsafe {
            let mut nursery = AlignedBuf([0u8; 4096]);
            let bytes = &mut nursery.0;
            write_lit(bytes, 0, 11);
            write_lit(bytes, LIT_SIZE, 22);
            let range = (bytes.as_ptr(), bytes.as_ptr().add(2 * LIT_SIZE));
            let mut old_space = OldSpace::new();
            let live = old_space.tenure(std::ptr::null_mut(), bytes.as_mut_ptr(), range);
            let retired = old_space.tenure(
                std::ptr::null_mut(),
                bytes.as_mut_ptr().add(LIT_SIZE),
                range,
            );
            let live_before = live.current();
            let retired_before = retired.current();
            let bytes_before = old_space.bytes_used();
            let plan = old_space.stage_compaction(&[live.addr()]).unwrap();
            let machine_state = crate::machine_state::MachineState::new();

            let result = old_space.commit_compaction(
                &machine_state,
                plan,
                &[live.addr(), retired.addr()],
                (std::ptr::null(), std::ptr::null()),
            );
            assert!(result.is_err());
            assert_eq!(live.current(), live_before);
            assert_eq!(retired.current(), retired_before);
            assert_eq!(old_space.bytes_used(), bytes_before);
            assert!(old_space.contains(live_before));
            assert!(old_space.contains(retired_before));
        }
    }
}
