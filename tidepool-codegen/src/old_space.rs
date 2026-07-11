//! Old-space (gen-1) tenuring for the value plane (Wave 1.A, component E).
//!
//! The session heap is split into two generations:
//!
//! - **gen-0 (nursery):** the bump-allocated region the JIT allocates into;
//!   collected by the minor (Cheney) GC on every `gc_trigger`.
//! - **gen-1 (`OldSpace`):** an append-only, growable buffer holding *tenured*
//!   bindings — strict-forced, immutable, first-order values promoted out of the
//!   nursery once at bind time so a later run can resolve them through a stable
//!   [`RootSlot`].
//!
//! ## The write barrier (static remembered set)
//!
//! A generational collector normally needs a write barrier to catch gen-1 →
//! gen-0 pointers (old objects mutated to point at young ones). Tier0 data
//! needs none: it is **strict-forced to normal form** (Wave 1.B component K)
//! and **immutable**, and [`OldSpace::tenure`] copies the value's *entire*
//! transitive closure into old-space at once — so its tenured graph holds NO
//! pointers back into the nursery. But Tier1 closures tenure UNFORCED, so
//! live (unevaluated/blackhole) thunks reach old-space, and forcing one later
//! MUTATES it: the update writes an indirection to a result allocated in the
//! nursery. Those indirection cells are the ONLY post-tenure-writable slots
//! in old-space, and all of them are enumerable at tenure time — so `tenure`
//! registers each as a persistent GC root. That registration IS the write
//! barrier: every minor GC traces the cell, evacuates its (post-force)
//! nursery target, and rewrites the cell in place. (Field bug 2026-07-10:
//! before this, a forced tenured thunk's result was collected out from under
//! it — a timing-dependent SIGSEGV.)
//!
//! The minor GC's from-range is the nursery ONLY (`raw::cheney_copy`'s
//! `is_in_range` excludes old-space addresses), so tenured objects are never
//! scanned, moved, or evacuated by a minor collection. Their addresses are
//! therefore stable for the session's life. Any OTHER future mutation path
//! into old-space (e.g. `writeSmallArray#` on a tenured array) breaks this
//! model and must add its own remembered-set entry; document it here.
//!
//! Old-space is compacted only on an explicit *major* pass (when a binding
//! generation dies) — never during a minor GC.

#![allow(dead_code)]

use std::collections::HashSet;
use tidepool_heap::gc::raw::{cheney_copy, for_each_pointer_field};
use tidepool_heap::layout::{read_size, read_tag, TAG_FORWARDED};

use crate::context::VMContext;

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

/// Append-only, growable gen-1 region for tenured bindings.
///
/// ## Address stability
///
/// Backed by an **arena chain**: `arenas` is a `Vec<Vec<u8>>` where each inner
/// `Vec<u8>` is allocated once and never reallocated — only the outer Vec's
/// metadata (fat pointer) can move when the chain grows, but that does NOT move
/// the inner arena's byte data. `cursor` is a bump offset into the last arena.
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
    arenas: Vec<Vec<u8>>,
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
    /// Evacuates `ptr`'s *entire* transitive closure (so no tenured pointer
    /// points back into the nursery — the no-write-barrier invariant), returns
    /// a [`RootSlot`] holding the tenured root pointer, and registers that slot
    /// as a persistent GC root
    /// ([`crate::host_fns::register_persistent_root`]) so minor GCs keep it live
    /// and never strand it.
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
    /// happens during (its `machine_state` is where the persistent root gets
    /// registered — see `host_fns::register_persistent_root`); the returned
    /// slot is registered as a persistent root and must outlive every
    /// fragment compiled against it (machine lifetime).
    pub unsafe fn tenure(
        &mut self,
        vmctx: *mut VMContext,
        ptr: *mut u8,
        nursery_from: (*const u8, *const u8),
    ) -> RootSlot {
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
            let free = self
                .arenas
                .last()
                .map_or(0, |a| a.len().saturating_sub(self.cursor));
            if free < needed {
                self.arenas.push(vec![0u8; DEFAULT_ARENA.max(needed)]);
                self.cursor = 0;
            }

            // Copy the closure into the current arena via Cheney's algorithm.
            // to_slice is wholly within a stable inner-arena allocation.
            let arena = self.arenas.last_mut().unwrap();
            let to_slice = &mut arena[self.cursor..self.cursor + needed];

            let res = cheney_copy(&[&mut root as *mut *mut u8], from_start, from_end, to_slice);

            debug_assert_eq!(
                res.bytes_copied, needed,
                "tenure: measure_closure_bytes({needed}) ≠ cheney_copy bytes_copied({})",
                res.bytes_copied
            );

            // The write barrier (see the module doc): any live thunk in the
            // graph just copied gets its indirection cell registered as a
            // persistent root, here — the only point where every such cell
            // in this tenure is enumerable at once.
            let base = to_slice.as_mut_ptr();
            let mut off = 0usize;
            while off < res.bytes_copied {
                let obj = base.add(off);
                let sz = read_size(obj) as usize;
                if read_tag(obj) == tidepool_heap::layout::TAG_THUNK {
                    let state = *obj.add(tidepool_heap::layout::THUNK_STATE_OFFSET);
                    if state == tidepool_heap::layout::THUNK_UNEVALUATED
                        || state == tidepool_heap::layout::THUNK_BLACKHOLE
                    {
                        crate::host_fns::register_persistent_root(
                            vmctx,
                            obj.add(tidepool_heap::layout::THUNK_INDIRECTION_OFFSET)
                                as *mut *mut u8,
                        );
                    }
                }
                off += (sz + 7) & !7;
            }

            self.cursor += res.bytes_copied;
            self.used += res.bytes_copied;
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
        // `measure == bytes_copied` invariant (latent until the Wave-1.B bind
        // path tenures multiple values per generation).
        if read_tag(obj) == TAG_FORWARDED {
            continue;
        }
        let size = read_size(obj) as usize;
        let aligned = (size + 7) & !7;
        total += aligned;

        for_each_pointer_field(obj, |field_slot| {
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

    /// Regression for the shipped `run_multi_bind` corruption: tenuring the
    /// SAME top-level root pointer twice in one call (e.g.
    /// `(a, b) <- pure (dup, dup)`, where both tuple fields are literally the
    /// same heap object). Unlike `test_overlapping_tenures_preserve_sharing`
    /// (where the shared node is a CHILD reached through a field —  already
    /// handled by `cheney_copy`/`evacuate`'s own forwarding check), this is
    /// the root of the SECOND `tenure()` call itself already being forwarded
    /// by the first. Before the forward-skip fix, `measure_closure_bytes`
    /// returns 0 for an already-forwarded root, so `tenure` rooted the raw
    /// (now TAG_FORWARDED) pointer directly — the resulting slot read back
    /// `heap tag: 255` instead of the tenured value.
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
}
