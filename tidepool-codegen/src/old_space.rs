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
//! ## Why no write barrier
//!
//! A generational collector normally needs a write barrier to catch gen-1 →
//! gen-0 pointers (old objects mutated to point at young ones). We need none:
//! tenured values are **strict-forced to normal form** (Wave 1.B component K)
//! and **immutable**, and [`OldSpace::tenure`] copies the value's *entire*
//! transitive closure into old-space at once — so a tenured object graph holds
//! NO pointers back into the nursery. The minor GC's from-range is the nursery
//! ONLY (`raw::cheney_copy`'s `is_in_range` excludes old-space addresses), so
//! tenured objects are never scanned, moved, or evacuated by a minor collection.
//! Their addresses are therefore stable for the session's life. This invariant
//! is load-bearing; document any future mutation path that would break it.
//!
//! Old-space is compacted only on an explicit *major* pass (when a binding
//! generation dies) — never during a minor GC.
//!
//! SCAFFOLD (Wave 0): signatures frozen; bodies land in Wave 1.A (Worker-Tenure).

#![allow(dead_code)]

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
/// Owns its backing buffer (machine-owned, dropped with the session). Grows by
/// reallocation when a `tenure` would overflow; growth must NOT invalidate the
/// stable addresses of already-tenured objects (see Wave 1.A spec).
pub struct OldSpace {
    // Wave 1.A (Worker-Tenure): backing storage for tenured objects + the
    // append cursor. Shape is the implementer's choice provided the address
    // stability + no-cross-generation-pointer invariants hold.
    _private: (),
}

impl Default for OldSpace {
    fn default() -> Self {
        Self::new()
    }
}

impl OldSpace {
    /// Create an empty old-space.
    pub fn new() -> Self {
        OldSpace { _private: () }
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
    /// [`crate::host_fns::gc_active_range`]); objects outside it are already
    /// stable (old-space, static, poison) and are left untouched.
    ///
    /// Idempotent per object via forwarding pointers, exactly like
    /// `raw::cheney_copy`.
    ///
    /// # Safety
    /// `ptr` must be a valid heap object; `nursery_from` must bound the live
    /// nursery; the returned slot is registered as a persistent root and must
    /// outlive every fragment compiled against it (machine lifetime).
    pub unsafe fn tenure(&mut self, ptr: *mut u8, nursery_from: (*const u8, *const u8)) -> RootSlot {
        let _ = (ptr, nursery_from);
        todo!("Wave 1.A (Worker-Tenure): transitive evacuate nursery→old_space + register persistent root (component E)")
    }

    /// Total bytes currently tenured (test/diagnostic accessor).
    pub fn bytes_used(&self) -> usize {
        todo!("Wave 1.A (Worker-Tenure)")
    }
}
