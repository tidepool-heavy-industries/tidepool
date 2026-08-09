//! GC roots, the per-machine GC/nursery state, and the `gc_trigger` slow path
//! (frame walk + Cheney copy) called from JIT allocation sites.
//!
//! GC-cluster reach (leaf 3): `GcState` and the two root registries
//! (run-scoped `rust_roots`, session-scoped `persistent_roots`) live on
//! [`crate::machine_state::MachineState`], reached EXCLUSIVELY through
//! `VMContext.machine_state` — never the per-thread `CURRENT_MACHINE` slot.
//! See the "GC-cluster reach" note on `machine_state.rs` for why: a write
//! (register) and the read that later traces it (`perform_gc`) must key on
//! the identical machine, and a per-thread slot can diverge from the `vmctx`
//! a given call actually belongs to. The functions below that take a
//! `vmctx: *mut VMContext` no-op (return / 0 / empty) when `vmctx` is null or
//! `(*vmctx).machine_state` is null — see
//! [`crate::machine_state::machine_state_opt`] and the null-vmctx invariant
//! documented on `RootScope`/`heap_to_value` in `heap_bridge.rs`.
//!
//! Isolation invariant: per-machine (not process-global) GC state is what
//! lets `MAX_CONCURRENT_EVALS` machines run concurrently on separate threads
//! without one eval's collector walking (and relocating objects in) another
//! eval's heap. A process-global GC pointer is forbidden in this module.

use crate::context::VMContext;
use crate::gc::frame_walker;
use crate::machine_state::{machine_state, machine_state_opt};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};

use super::cancel::check_cancel_and_set_error;

/// Register a Rust stack/heap slot containing a heap pointer as a GC root on
/// the machine `vmctx` belongs to. GC will update the slot's value in-place
/// if the pointed-to object moves. No-op when `vmctx`/`vmctx.machine_state`
/// is null (see the module doc).
///
/// # Safety
/// The slot must remain valid and dereferenceable until the matching
/// `truncate_rust_roots` (or `clear_rust_roots`) call.
pub unsafe fn register_rust_root(vmctx: *mut VMContext, slot: *mut *mut u8) {
    if let Some(ms) = machine_state_opt(vmctx) {
        ms.register_rust_root(slot);
    }
}

/// Current depth of the Rust-root stack. Pair with `truncate_rust_roots` to
/// scope registrations: host fns that call back into JIT code can nest (e.g.
/// `heap_force` → thunk code → `heap_force`), so unscoped clearing would drop
/// an outer frame's registrations. Returns 0 when there is no machine to read.
///
/// # Safety
/// If `vmctx` is non-null, it must point to a live `VMContext`.
pub unsafe fn rust_roots_mark(vmctx: *mut VMContext) -> usize {
    machine_state_opt(vmctx)
        .map(|ms| ms.rust_roots_len())
        .unwrap_or(0)
}

/// Drop roots registered after `mark`, preserving outer registrations.
///
/// # Safety
/// If `vmctx` is non-null, it must point to a live `VMContext`.
pub unsafe fn truncate_rust_roots(vmctx: *mut VMContext, mark: usize) {
    if let Some(ms) = machine_state_opt(vmctx) {
        ms.truncate_rust_roots(mark);
    }
}

/// Remove all registered Rust roots. Call after the GC-unsafe region ends.
///
/// # Safety
/// If `vmctx` is non-null, it must point to a live `VMContext`.
pub unsafe fn clear_rust_roots(vmctx: *mut VMContext) {
    if let Some(ms) = machine_state_opt(vmctx) {
        ms.clear_rust_roots();
    }
}

/// Register a SESSION-SCOPED GC root slot (Wave 1.A, component D) on the
/// machine `vmctx` belongs to.
///
/// Unlike [`register_rust_root`] (run-scoped, cleared every `RegistryGuard`
/// drop), a persistent root survives across runs and is cleared only by
/// `free_session_heap` at machine drop. `perform_gc` appends these to the
/// root set on every collection, so the slot's stored pointer is kept live and
/// rewritten in place when the pointee moves. No-op when
/// `vmctx`/`vmctx.machine_state` is null (see the module doc).
///
/// # Safety
/// `slot` must be non-null, point to a valid `*mut u8` heap-pointer location,
/// and remain valid + dereferenceable until `free_session_heap` runs (the
/// owning `JitEffectMachine` drops). A slot freed or moved before that is a
/// use-after-free the GC will trip on.
pub unsafe fn register_persistent_root(vmctx: *mut VMContext, slot: *mut *mut u8) {
    if let Some(ms) = machine_state_opt(vmctx) {
        ms.register_persistent_root(slot);
    }
}

/// Number of registered persistent roots on the machine `vmctx` belongs to
/// (test/diagnostic accessor). Returns 0 when there is no machine to read.
///
/// # Safety
/// If `vmctx` is non-null, it must point to a live `VMContext`.
pub unsafe fn persistent_roots_count(vmctx: *mut VMContext) -> usize {
    machine_state_opt(vmctx)
        .map(|ms| ms.persistent_roots_count())
        .unwrap_or(0)
}

/// Arm the write barrier on the machine `vmctx` belongs to. Idempotent;
/// `OldSpace::tenure` calls this unconditionally on every tenure. No-op when
/// `vmctx`/`vmctx.machine_state` is null (see the module doc).
///
/// # Safety
/// If `vmctx` is non-null, it must point to a live `VMContext`.
pub unsafe fn arm_write_barrier(vmctx: *mut VMContext) {
    if let Some(ms) = machine_state_opt(vmctx) {
        ms.arm_write_barrier();
    }
}

/// Register a newly-allocated old-space arena's byte range on the machine
/// `vmctx` belongs to (diagnostic reach — see the field doc on
/// `MachineState::old_space_arenas`). No-op when `vmctx`/`vmctx.machine_state`
/// is null.
///
/// # Safety
/// If `vmctx` is non-null, it must point to a live `VMContext`; `start`/`end`
/// must bound a live allocation that outlives every future
/// `retire_old_space_arena` call for the same range.
pub unsafe fn register_old_space_arena(vmctx: *mut VMContext, start: *const u8, end: *const u8) {
    if let Some(ms) = machine_state_opt(vmctx) {
        ms.register_old_space_arena(start, end);
    }
}

/// Number of remembered write-barrier slots on the machine `vmctx` belongs to
/// (test/diagnostic accessor). Returns 0 when there is no machine to read.
///
/// # Safety
/// If `vmctx` is non-null, it must point to a live `VMContext`.
pub unsafe fn remembered_slots_count(vmctx: *mut VMContext) -> usize {
    machine_state_opt(vmctx)
        .map(|ms| ms.remembered_slots_count())
        .unwrap_or(0)
}

/// Process-global test override: forces `write_barrier` to no-op (as if
/// unarmed) regardless of the machine's actual armed state. Default off. This
/// is the mutation-check kill switch (#[doc(hidden)], test-only) — flipping
/// it on and re-running a barrier-dependent test must reproduce the SAME
/// pre-fix corruption signature, or the test proves nothing.
static WRITE_BARRIER_DISABLED_FOR_TEST: AtomicBool = AtomicBool::new(false);

/// Test-only: disable (or re-enable) the write barrier process-wide,
/// independent of any machine's armed state. Not part of the public API.
#[doc(hidden)]
pub fn set_write_barrier_disabled_for_test(on: bool) {
    WRITE_BARRIER_DISABLED_FOR_TEST.store(on, Ordering::Relaxed);
}

fn write_barrier_disabled_for_test() -> bool {
    WRITE_BARRIER_DISABLED_FOR_TEST.load(Ordering::Relaxed)
}

/// THE write barrier (see `old_space.rs`'s module doc for the invariant):
/// every store of a possibly-young pointer into an already-tenured or
/// external-to-nursery location routes through this ONE function —
/// `OldSpace::tenure`'s thunk-indirection cells, `WriteSmallArray`/
/// `WriteArray`, `casSmallArray#`, and the boxed-array copy family's
/// destination range. `slot` is the ADDRESS of the pointer-sized location
/// that was just written (or, for tenure, a thunk's indirection cell) — NOT
/// the value stored there. Records `slot` in the machine's remembered set so
/// `perform_gc` traces and rewrites it on every collection, exactly like a
/// stack or persistent root.
///
/// Cheap when unarmed: a relaxed load and return, before any hashing or
/// `RefCell` borrow — same shape as `maybe_raise_gc_fault`'s disarmed check.
/// Sound to skip while unarmed: before the first `OldSpace::tenure` call
/// there is no old-space, so no old-to-young store is possible yet.
///
/// `extern "C"` so the JIT can call it directly from emitted
/// `WriteSmallArray`/`WriteArray` IR (no Rust host-fn wrapper in between);
/// Rust call sites (`OldSpace::tenure`, the CAS/copy host fns) call the exact
/// same function. No-op when `vmctx`/`vmctx.machine_state` is null (see the
/// module doc) — same null-vmctx invariant as `register_persistent_root`.
///
/// # Safety
/// `slot` must be non-null and point to a valid, dereferenceable
/// `*mut u8`-sized location that stays valid until the memory holding it dies
/// (arena teardown / machine drop forgets it — see `forget_remembered_range`).
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn write_barrier(vmctx: *mut VMContext, slot: *mut *mut u8) {
    if write_barrier_disabled_for_test() {
        return;
    }
    // SAFETY: vmctx is valid; machine_state was installed before entering
    // JIT code (same contract as every other vmctx-reached GC-cluster fn).
    if let Some(ms) = unsafe { machine_state_opt(vmctx) } {
        if !ms.write_barrier_armed() {
            return;
        }
        ms.register_remembered_slot(slot);
    }
}

/// Per-machine state for the copying garbage collector.
pub(crate) struct GcState {
    pub active_start: *mut u8,
    pub active_size: usize,
    /// `Vec<u64>`, not `Vec<u8>` (L8, repo-review-2026-07-06/01-gc-memory-
    /// safety.md) — see `SessionState::heap`'s doc (jit_machine.rs) for why.
    pub active_buffer: Option<Vec<u64>>,
}

/// A zeroed byte buffer at least `size` bytes, 8-byte aligned by
/// construction (L8: backed by `Vec<u64>`, not `Vec<u8>` — see
/// `GcState::active_buffer`'s doc for why).
fn alloc_aligned_zeroed(size: usize) -> Vec<u64> {
    vec![0u64; size.div_ceil(8)]
}

/// Byte-slice view over a `Vec<u64>` buffer, for callers (like
/// `cheney_copy`) that want `&mut [u8]`. Always sound: `u8` has no
/// alignment/validity requirements a `u64` buffer doesn't already satisfy.
fn as_bytes_mut(words: &mut [u64]) -> &mut [u8] {
    // SAFETY: `words` is a valid, initialized `&mut [u64]` for its full
    // byte length; reinterpreting as `&mut [u8]` only weakens alignment
    // requirements and every `u64` is already a valid sequence of 8 `u8`s.
    unsafe {
        std::slice::from_raw_parts_mut(words.as_mut_ptr() as *mut u8, std::mem::size_of_val(words))
    }
}

// SAFETY: GcState contains raw pointers but is only accessed from the thread
// driving the owning MachineState's machine.
unsafe impl Send for GcState {}

/// GC trigger: called by JIT code when alloc_ptr exceeds alloc_limit.
///
/// This function MUST be compiled with frame pointers preserved
/// (the whole crate uses preserve_frame_pointers, and the Rust profile
/// should have force-frame-pointers = true for the gc path).
///
/// The frame walker in gc_trigger reads RBP to walk the JIT stack.
#[inline(never)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn gc_trigger(vmctx: *mut VMContext) {
    // Force a frame to be created
    let mut _dummy = [0u64; 2];
    std::hint::black_box(&mut _dummy);

    GC_TRIGGER_CALL_COUNT.fetch_add(1, Ordering::SeqCst);
    GC_TRIGGER_LAST_VMCTX.store(vmctx as usize, Ordering::SeqCst);

    // External cancellation safepoint. Record `RuntimeError::Cancelled`
    // and skip `perform_gc`: the JIT's slow-path post-GC re-check will
    // fail (alloc_ptr/alloc_limit are unchanged), routing the next
    // allocation through `runtime_oom`'s poison path. `runtime_oom`'s
    // first-write-wins `set_first_cause` preserves the `Cancelled` cause so
    // the unwind surfaces it via the boundary's `surface_error` resolution,
    // not as `HeapOverflow`.
    //
    // Post-OOM stores into the poison are bounded by `POISON_BUF_SIZE`
    // (16 KiB, sized for worst-case Con writes — see PR #272).
    //
    // The other cancel safepoints — the trampoline loop, the join back-edge
    // (`runtime_cancel_check`, #325), and the effect-dispatch boundary in
    // `drive_to_done` — already give prompt unwind for tail-recursive,
    // join-looping, and effect-driven programs; this path closes the gap for
    // pure non-tail-call allocator loops that never reach any of them (#273).
    // Same shared check as those safepoints; this one just returns void and
    // skips perform_gc (the post-GC re-check routes the next allocation
    // through runtime_oom's poison path, per the comment above).
    if check_cancel_and_set_error(vmctx) {
        return;
    }

    #[cfg(target_arch = "x86_64")]
    {
        let fp: usize;
        // SAFETY: Reading the frame pointer register (RBP) via inline asm.
        // nomem/nostack options are correct — this is a pure register read.
        unsafe {
            std::arch::asm!("mov {}, rbp", out(reg) fp, options(nomem, nostack));
        }
        perform_gc(fp, vmctx);
    }

    #[cfg(target_arch = "aarch64")]
    {
        let fp: usize;
        // SAFETY: Reading the frame pointer register (x29) via inline asm.
        unsafe {
            std::arch::asm!("mov {}, x29", out(reg) fp, options(nomem, nostack));
        }
        perform_gc(fp, vmctx);
    }
}

/// Shared GC body: walk frames, run Cheney copy, call hooks.
#[inline(never)]
/// Heap growth ceiling. Defaults to 1 GiB; override with `TIDEPOOL_MAX_HEAP`
/// (bytes). Reaching the cap with a full live set ends in a clean
/// `HeapOverflow` via the post-GC allocation re-check, never a signal.
fn max_heap_bytes() -> usize {
    use std::sync::OnceLock;
    static CAP: OnceLock<usize> = OnceLock::new();
    *CAP.get_or_init(|| {
        std::env::var("TIDEPOOL_MAX_HEAP")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1 << 30)
    })
}

/// Test override state for an env-gated diagnostic knob: 0 = unset (defer to
/// the environment), 1 = force on, 2 = force off.
///
/// Tri-state rather than a bool because `env::set_var` is racy against the
/// `OnceLock`-cached env reads below (they latch the FIRST read) and unsafe on
/// edition 2024, AND because a test must be able to reach BOTH states. An
/// OR-only override can add force-on but never retract the env var, which
/// leaves a suite run under `TIDEPOOL_HEAP_VERIFY=1` unable to exercise any
/// behavior that requires the verifier off — `gc_write_barrier.rs`'s
/// barrier mutation check needs exactly that, since with the verifier on the
/// tenured-graph pass detects the corruption first and aborts.
const OVERRIDE_UNSET: u8 = 0;
const OVERRIDE_ON: u8 = 1;
const OVERRIDE_OFF: u8 = 2;

fn resolve_override(override_state: &AtomicU8, env_cached: bool) -> bool {
    match override_state.load(Ordering::Relaxed) {
        OVERRIDE_ON => true,
        OVERRIDE_OFF => false,
        _ => env_cached,
    }
}

/// Process-global test override for `heap_verify_enabled`.
static HEAP_VERIFY_FORCE: AtomicU8 = AtomicU8::new(OVERRIDE_UNSET);

/// Test-only: force the post-GC heap verifier on or off, independent of
/// `TIDEPOOL_HEAP_VERIFY` in EITHER direction. Not part of the public API.
/// Forwards to `tidepool-heap`'s `set_checked_scanning` so one knob drives
/// both crates' diagnostic scanning.
#[doc(hidden)]
pub fn set_heap_verify(on: bool) {
    HEAP_VERIFY_FORCE.store(
        if on { OVERRIDE_ON } else { OVERRIDE_OFF },
        Ordering::Relaxed,
    );
    tidepool_heap::gc::raw::set_checked_scanning(on);
}

/// Test-only: clear the heap-verify override and defer back to
/// `TIDEPOOL_HEAP_VERIFY`. Not part of the public API.
#[doc(hidden)]
pub fn clear_heap_verify_override() {
    HEAP_VERIFY_FORCE.store(OVERRIDE_UNSET, Ordering::Relaxed);
    tidepool_heap::gc::raw::clear_checked_scanning_override();
}

/// Kill-switched fail-loud mode: `TIDEPOOL_HEAP_VERIFY=1` (or `set_heap_verify`)
/// walks the entire live set after every GC and panics on the first invariant
/// violation. Tests opt in; production pays one cached env read.
fn heap_verify_enabled() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    let env = *ON.get_or_init(|| std::env::var("TIDEPOOL_HEAP_VERIFY").is_ok_and(|v| v == "1"));
    resolve_override(&HEAP_VERIFY_FORCE, env)
}

/// Process-global test override for `gc_poison_enabled` (same tri-state
/// rationale as `HEAP_VERIFY_FORCE`).
static GC_POISON_FORCE: AtomicU8 = AtomicU8::new(OVERRIDE_UNSET);

/// Test-only: force from-space poisoning on or off, independent of
/// `TIDEPOOL_GC_POISON` in either direction. Not part of the public API.
#[doc(hidden)]
pub fn set_gc_poison(on: bool) {
    GC_POISON_FORCE.store(
        if on { OVERRIDE_ON } else { OVERRIDE_OFF },
        Ordering::Relaxed,
    );
}

/// Test-only: clear the gc-poison override and defer back to
/// `TIDEPOOL_GC_POISON`. Not part of the public API.
#[doc(hidden)]
pub fn clear_gc_poison_override() {
    GC_POISON_FORCE.store(OVERRIDE_UNSET, Ordering::Relaxed);
}

/// Kill-switched fail-loud mode: `TIDEPOOL_GC_POISON=1` (or `set_gc_poison`)
/// fills from-space with 0xDD after every collection, before the buffer is
/// freed. The post-GC verifier walks TO-SPACE, so it cannot see the missed-
/// stack-root class: an object whose only reference was skipped by the frame
/// walk is never evacuated, and the stale slot keeps pointing into freed
/// from-space — crashing only IF the allocator reuses those pages (the
/// timing-dependent-SIGSEGV signature). Poisoning makes any read through such
/// a slot deterministic: tag 0xDD is unknown, so it fails loudly at the next
/// dereference or the next verified GC instead of sometimes working.
fn gc_poison_enabled() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    let env = *ON.get_or_init(|| std::env::var("TIDEPOOL_GC_POISON").is_ok_and(|v| v == "1"));
    resolve_override(&GC_POISON_FORCE, env)
}

/// Count of completed `verify_heap_post_gc` runs, process-wide. Lets a test
/// prove the verifier actually fired rather than silently no-op'ing.
static HEAP_VERIFY_RUNS: AtomicUsize = AtomicUsize::new(0);

/// Test-only: how many times `verify_heap_post_gc` has run in this process.
#[doc(hidden)]
pub fn heap_verify_run_count() -> usize {
    HEAP_VERIFY_RUNS.load(Ordering::Relaxed)
}

/// Count of completed heap-doubling passes (`perform_gc`'s doubling branch),
/// process-wide. Lets a test prove a collection actually took the doubling
/// path rather than assuming a nursery size forces it.
static GC_DOUBLING_RUNS: AtomicUsize = AtomicUsize::new(0);

/// Test-only: how many times the heap-doubling branch has run in this process.
#[doc(hidden)]
pub fn gc_doubling_run_count() -> usize {
    GC_DOUBLING_RUNS.load(Ordering::Relaxed)
}

/// Verdict for a pointer value read from a live-heap slot, post-GC.
enum FieldPtrVerdict {
    /// Null (legal mid-LetRec construction), inside to-space and 8-aligned,
    /// or outside every space this collection touched (a poison object, a
    /// malloc'd byte array, or any other address this walk has no opinion
    /// on).
    Ok,
    /// Lands inside a `retired` range: a dangling evacuation.
    DanglingIntoRetired,
    /// Lands inside to-space but is not 8-aligned.
    MisalignedToSpace,
}

/// The single owner for "is this pointer target acceptable post-GC" — every
/// slot classification in [`verify_heap_post_gc`] goes through this, so a
/// future pass over a different slot population (e.g. old-space's external
/// array payloads) can call the same predicate instead of reimplementing the
/// to-space/retired-range comparison.
fn classify_field_ptr(
    p: *const u8,
    to_start: *const u8,
    to_end: *const u8,
    retired: &[(*const u8, *const u8)],
) -> FieldPtrVerdict {
    if p.is_null() {
        return FieldPtrVerdict::Ok;
    }
    if p >= to_start && p < to_end {
        return if (p as usize).is_multiple_of(8) {
            FieldPtrVerdict::Ok
        } else {
            FieldPtrVerdict::MisalignedToSpace
        };
    }
    if retired.iter().any(|&(start, end)| p >= start && p < end) {
        return FieldPtrVerdict::DanglingIntoRetired;
    }
    FieldPtrVerdict::Ok
}

/// Post-GC walk of the TENURED graph, reachable from this machine's
/// persistent roots (see `heap_verify_enabled`).
///
/// A minor collection never scans old-space, and `verify_heap_post_gc` walks
/// only the packed to-space it just produced — so neither sees a pointer slot
/// living in old-space or in a boxed array's external malloc'd payload
/// buffer. This pass covers exactly that population: starting from each
/// persistent root (a tenured binding's stable slot), it follows the object
/// graph and classifies every pointer slot `for_each_pointer_field` yields,
/// including a `TAG_LIT` array wrapper's payload slots.
///
/// It is deliberately INDEPENDENT of the write barrier. The barrier's
/// remembered set enumerates only the stores the barrier itself recorded, so
/// a verifier built on it would inherit the barrier's blind spot and could
/// never see the failure that matters — a store the barrier MISSED. Following
/// the object graph instead means an unrecorded old-to-young store still
/// leaves a slot pointing into a retired range, and it fails loudly here, at
/// the collection that stranded it, instead of as a tag-221 case trap
/// whenever something next dereferences it.
///
/// Recursion is confined to memory that is safe to read: a target is followed
/// only when it lies in to-space or inside a live old-space arena. Targets
/// outside both (external payload buffers, poison, statics) are classified
/// but never dereferenced, and a target in a `retired` range fails before any
/// dereference — that memory is freed and possibly poisoned.
///
/// # Safety
/// `persistent_roots` must hold valid, registered root slots; `arenas` must
/// bound live old-space allocations; `retired` ranges are compared, never
/// dereferenced.
unsafe fn verify_tenured_graph(
    persistent_roots: &[*mut *mut u8],
    arenas: &[(*const u8, *const u8)],
    to_start: *const u8,
    to_end: *const u8,
    retired: &[(*const u8, *const u8)],
) {
    let in_arena = |p: *const u8| arenas.iter().any(|&(start, end)| p >= start && p < end);
    let readable = |p: *const u8| (p >= to_start && p < to_end) || in_arena(p);

    let fail = |owner: *const u8, target: *const u8, what: &str| -> ! {
        panic!(
            "[HEAP VERIFY] tenured-graph violation after GC: {what}\n  \
             slot owner object at {owner:p}, target {target:p}\n  \
             retired ranges were {retired:?}, to-space {to_start:p}..{to_end:p}\n  \
             a tenured slot pointing into a retired range means an old-to-young \
             store was never recorded by the write barrier (see old_space.rs)"
        )
    };

    let mut visited: std::collections::HashSet<*mut u8> = std::collections::HashSet::new();
    let mut work: Vec<*mut u8> = Vec::new();

    for &slot in persistent_roots {
        let root = *slot;
        if !root.is_null() && readable(root as *const u8) {
            work.push(root);
        }
    }

    while let Some(obj) = work.pop() {
        if !visited.insert(obj) {
            continue;
        }
        tidepool_heap::gc::raw::for_each_pointer_field(obj, |field_slot| {
            let target = *field_slot;
            match classify_field_ptr(target as *const u8, to_start, to_end, retired) {
                FieldPtrVerdict::Ok => {}
                FieldPtrVerdict::DanglingIntoRetired => fail(
                    obj as *const u8,
                    target as *const u8,
                    "a slot reachable from a tenured binding points into a RETIRED space \
                     (dangling old-to-young reference)",
                ),
                FieldPtrVerdict::MisalignedToSpace => fail(
                    obj as *const u8,
                    target as *const u8,
                    "a slot reachable from a tenured binding holds a misaligned to-space pointer",
                ),
            }
            if !target.is_null() && readable(target as *const u8) && !visited.contains(&target) {
                work.push(target);
            }
        });
    }
}

/// Post-GC check that every slot the write barrier remembered still holds an
/// acceptable target (see `heap_verify_enabled`).
///
/// Defense-in-depth on the barrier's own tracing, NOT on its coverage: a
/// remembered slot is handed to `perform_gc` as a root, so its target must
/// have been evacuated and the slot rewritten. A stale target here means the
/// GC mishandled a root it was given — a different failure from the barrier
/// failing to record the slot at all, which is [`verify_tenured_graph`]'s job.
///
/// # Safety
/// `slots` must hold valid remembered-slot addresses; `retired` ranges are
/// compared, never dereferenced.
unsafe fn verify_remembered_slots(
    slots: &[*mut *mut u8],
    to_start: *const u8,
    to_end: *const u8,
    retired: &[(*const u8, *const u8)],
) {
    for &slot in slots {
        let target = *slot;
        match classify_field_ptr(target as *const u8, to_start, to_end, retired) {
            FieldPtrVerdict::Ok => {}
            FieldPtrVerdict::DanglingIntoRetired => panic!(
                "[HEAP VERIFY] remembered slot {slot:p} holds a RETIRED-space pointer \
                 {target:p} after GC — the barrier recorded this slot, so the collector \
                 was handed it as a root and should have rewritten it\n  \
                 retired ranges were {retired:?}, to-space {to_start:p}..{to_end:p}"
            ),
            FieldPtrVerdict::MisalignedToSpace => panic!(
                "[HEAP VERIFY] remembered slot {slot:p} holds a misaligned to-space \
                 pointer {target:p} after GC"
            ),
        }
    }
}

/// Post-GC heap invariant walk (see `heap_verify_enabled`).
///
/// Walks the packed live set `to_start..+live_bytes` exactly like the Cheney
/// scan and validates every object:
/// - known tag (a FORWARDED header surviving into to-space is corruption);
/// - header size consistent with the tag (for Cons: `24 + 8*num_fields`
///   EXACTLY — catches the u16 size-wrap class, S3-C1/C2);
/// - Lit tags within the known set (catches constant drift, S3-C3);
/// - thunk state bytes valid;
/// - every pointer field classifies as [`FieldPtrVerdict::Ok`] via
///   [`classify_field_ptr`] — a pointer into a `retired` range is a dangling
///   evacuation and fails loudly here instead of as a SIGSEGV collections
///   later. BLACKHOLE capture slots are checked too, as defense-in-depth
///   alongside the general field walk: `for_each_pointer_field` traces
///   `THUNK_UNEVALUATED`/`THUNK_BLACKHOLE` captures identically (the S3-C6
///   skip was fixed in `raw.rs`, 2026-06-11), so a from-space capture here
///   would now be caught by the main Cheney scan too — this check just
///   guards the invariant a second way rather than covering a live gap.
///
/// `retired` covers every space THIS collection evacuated OUT of: the
/// original nursery, and, on the heap-doubling path, the intermediate
/// to-space as well (doubling runs a second Cheney pass over what was, for
/// that pass, itself a from-space — a pointer dangling into it is exactly as
/// much a dangling evacuation as one into the original nursery). Addresses in
/// `retired` are COMPARED, never dereferenced — the buffer may already be
/// freed by the time this runs. That's sound because nothing allocates
/// between a range's free and this call, and every `retired` range was
/// allocated while still live and is disjoint from `to_start..+live_bytes`
/// and from every other `retired` range (each doubling pass allocates a
/// fresh, larger buffer before the prior one is dropped).
///
/// Scope: this walk covers only the packed to-space `perform_gc` just
/// produced. Old-space and boxed arrays' external malloc'd payload buffers
/// are covered separately by [`verify_tenured_graph`], which `perform_gc`
/// runs immediately after this.
unsafe fn verify_heap_post_gc(
    to_start: *const u8,
    live_bytes: usize,
    retired: &[(*const u8, *const u8)],
) {
    HEAP_VERIFY_RUNS.fetch_add(1, Ordering::Relaxed);
    use crate::layout as l;
    let to_end = to_start.add(live_bytes);

    let fail = |off: usize, idx: usize, what: &str, obj: *const u8| -> ! {
        let dump_len = 32.min(live_bytes - off);
        let bytes = std::slice::from_raw_parts(obj, dump_len);
        panic!(
            "[HEAP VERIFY] violation after GC: {what}\n  object #{idx} at to-space offset {off:#x} \
             (live_bytes={live_bytes:#x})\n  first {dump_len} bytes: {bytes:02x?}\n  \
             retired ranges were {retired:?}, to-space {to_start:p}..{to_end:p}"
        )
    };

    let check_field = |off: usize, idx: usize, obj: *const u8, slot: usize, label: &str| {
        let p = *(obj.add(slot) as *const *const u8);
        match classify_field_ptr(p, to_start, to_end, retired) {
            FieldPtrVerdict::Ok => {}
            FieldPtrVerdict::DanglingIntoRetired => fail(
                off,
                idx,
                &format!(
                    "{label} slot +{slot} holds a FROM-SPACE pointer {p:p} (dangling evacuation)"
                ),
                obj,
            ),
            FieldPtrVerdict::MisalignedToSpace => fail(
                off,
                idx,
                &format!("{label} slot +{slot} holds a misaligned to-space pointer {p:p}"),
                obj,
            ),
        }
    };

    let mut off = 0usize;
    let mut idx = 0usize;
    while off < live_bytes {
        let obj = to_start.add(off);
        let tag = *obj;
        // Size is a u32 at byte offset 1 — intentionally unaligned in the
        // header layout; must be read_unaligned (debug builds abort on
        // misaligned derefs).
        let size = std::ptr::read_unaligned(obj.add(1) as *const u32) as usize;
        if size < 8 || off + size > live_bytes {
            fail(
                off,
                idx,
                &format!("size {size} out of bounds for tag {tag}"),
                obj,
            );
        }
        match tag {
            l::TAG_CON => {
                let nf = *(obj.add(l::CON_NUM_FIELDS_OFFSET as usize) as *const u16) as usize;
                let expect = 24 + 8 * nf;
                if size != expect {
                    fail(
                        off,
                        idx,
                        &format!("Con size {size} != 24 + 8*num_fields({nf}) = {expect} (size-wrap class)"),
                        obj,
                    );
                }
                for i in 0..nf {
                    check_field(
                        off,
                        idx,
                        obj,
                        l::CON_FIELDS_OFFSET as usize + 8 * i,
                        "Con field",
                    );
                }
            }
            l::TAG_LIT => {
                if size != l::LIT_TOTAL_SIZE as usize {
                    fail(
                        off,
                        idx,
                        &format!("Lit size {size} != {}", l::LIT_TOTAL_SIZE),
                        obj,
                    );
                }
                let lt = *obj.add(l::LIT_TAG_OFFSET as usize);
                if lt as i64 > l::LIT_TAG_ARRAY as i64 {
                    fail(
                        off,
                        idx,
                        &format!("unknown lit tag {lt} (constant drift?)"),
                        obj,
                    );
                }
                // SmallArray#/Array#: the value field points at a malloc'd,
                // GC-external payload `[u64 len][ptr0..ptrN]` (never itself
                // evacuated — see `for_each_pointer_field`'s TAG_LIT arm), but
                // its SLOT CONTENTS are ordinary heap pointers that must obey
                // the same from/to-space invariants as any other field.
                if lt == l::LIT_TAG_SMALLARRAY as u8 || lt == l::LIT_TAG_ARRAY as u8 {
                    let payload = *(obj.add(l::LIT_VALUE_OFFSET as usize) as *const *const u8);
                    if !payload.is_null() {
                        let len = *(payload as *const u64) as usize;
                        for i in 0..len {
                            let slot_addr = payload.add(8 + i * 8);
                            let p = *(slot_addr as *const *const u8);
                            match classify_field_ptr(p, to_start, to_end, retired) {
                                FieldPtrVerdict::Ok => {}
                                FieldPtrVerdict::DanglingIntoRetired => fail(
                                    off,
                                    idx,
                                    &format!(
                                        "array elem[{i}] holds a FROM-SPACE pointer {p:p} (dangling evacuation)"
                                    ),
                                    obj,
                                ),
                                FieldPtrVerdict::MisalignedToSpace => fail(
                                    off,
                                    idx,
                                    &format!(
                                        "array elem[{i}] holds a misaligned to-space pointer {p:p}"
                                    ),
                                    obj,
                                ),
                            }
                        }
                    }
                }
            }
            l::TAG_CLOSURE => {
                let nc = *(obj.add(l::CLOSURE_NUM_CAPTURED_OFFSET as usize) as *const u16) as usize;
                let min = l::CLOSURE_CAPTURED_OFFSET as usize + 8 * nc;
                if size < min {
                    fail(
                        off,
                        idx,
                        &format!("Closure size {size} < captures end {min} (num_captured={nc})"),
                        obj,
                    );
                }
                // A null code pointer is LEGAL mid-LetRec: Phase 1 pre-allocs
                // every closure in the group (header + num_captured, slots
                // zeroed) and Phase 3a fills code pointers — any GC point
                // between (the next binding's pre-alloc, a capture's
                // ensure_heap_ptr) sees this state. Same allowance as null Con
                // fields below. (Was a fail — it made HEAP_VERIFY false-positive
                // on any letrec caught mid-construction, field-hit 2026-07-10.)
                for i in 0..nc {
                    check_field(
                        off,
                        idx,
                        obj,
                        l::CLOSURE_CAPTURED_OFFSET as usize + 8 * i,
                        "Closure capture",
                    );
                }
            }
            l::TAG_THUNK => {
                let state = *obj.add(l::THUNK_STATE_OFFSET as usize);
                match state {
                    l::THUNK_UNEVALUATED => {
                        let n = (size - l::THUNK_CAPTURED_OFFSET as usize) / 8;
                        for i in 0..n {
                            check_field(
                                off,
                                idx,
                                obj,
                                l::THUNK_CAPTURED_OFFSET as usize + 8 * i,
                                "Thunk capture",
                            );
                        }
                    }
                    l::THUNK_EVALUATED => {
                        check_field(
                            off,
                            idx,
                            obj,
                            l::THUNK_INDIRECTION_OFFSET as usize,
                            "Thunk indirection",
                        );
                    }
                    l::THUNK_BLACKHOLE => {
                        // for_each_pointer_field traces THUNK_BLACKHOLE
                        // captures identically to THUNK_UNEVALUATED (S3-C6
                        // fixed in raw.rs, 2026-06-11) — this is a second,
                        // redundant check on the same invariant, not a gap.
                        let n = (size - l::THUNK_CAPTURED_OFFSET as usize) / 8;
                        for i in 0..n {
                            check_field(
                                off,
                                idx,
                                obj,
                                l::THUNK_CAPTURED_OFFSET as usize + 8 * i,
                                "BLACKHOLE capture",
                            );
                        }
                    }
                    other => fail(off, idx, &format!("invalid thunk state {other}"), obj),
                }
            }
            l::TAG_FORWARDED => fail(off, idx, "FORWARDED header in to-space", obj),
            other => fail(off, idx, &format!("unknown heap tag {other}"), obj),
        }
        off += (size + 7) & !7;
        idx += 1;
    }
}

fn perform_gc(fp: usize, vmctx: *mut VMContext) {
    // SAFETY: vmctx is valid; machine_state was installed before entering JIT code.
    let registry_ptr = unsafe { machine_state(vmctx) }.stack_map_registry();
    if let Some(registry_ptr) = registry_ptr {
        // SAFETY: registry_ptr was set by set_stack_map_registry and outlives JIT execution.
        let registry = unsafe { &*registry_ptr };
        // `stack_low` is a local in THIS frame. perform_gc is always called
        // beneath the JIT call chain (gc_trigger → perform_gc, never the
        // reverse), and the stack grows down, so this address is a sound
        // LOW bound: every JIT frame `walk_frames` is about to walk sits at
        // a strictly higher address than this one.
        let stack_low: u8 = 0;
        let bounds = frame_walker::StackBounds::capture(&stack_low as *const u8 as usize);
        // SAFETY: fp is a valid frame pointer read from gc_trigger's caller.
        // registry contains stack maps for all JIT functions in the call chain.
        // A violation of that contract is now a controlled failure, not UB —
        // see `walk_frames`'s doc.
        let roots =
            unsafe { frame_walker::walk_frames(fp, registry, bounds, heap_verify_enabled()) };

        // ── Cheney copying GC ──────────────────────────────
        // SAFETY: vmctx is valid; machine_state was installed before entering
        // JIT code (same contract as the stack_map_registry read above).
        let ms = unsafe { machine_state(vmctx) };
        // The `GcState` is TAKEN out of its cell for the duration of the
        // copy, not borrowed: a fault (SIGSEGV/SIGILL) anywhere in this
        // block siglongjmps out of this frame, abandoning the owned
        // `state`/`tospace`/`root_slots` locals on the dead stack — they
        // leak, nothing double-frees — and the cell is left EMPTY rather
        // than permanently marked mutably borrowed. Every teardown path
        // (`reclaim_session_heap`, `clear_run_scratch`, `free_session_heap`)
        // already treats an empty cell as the ordinary "no GC state" case.
        if let Some(mut state) = ms.take_gc_state() {
            // M3 (deep_force): a real collection is about to run — bump
            // so callers holding an address-keyed cache across this call
            // (e.g. deep_force's visited set) know to invalidate it.
            ms.bump_gc_generation();
            let from_start = state.active_start;
            let from_size = state.active_size;
            // SAFETY: from_start + from_size stays within the active GC region.
            let from_end = unsafe { from_start.add(from_size) };

            let mut tospace = alloc_aligned_zeroed(from_size);

            // Convert StackRoot to raw slot pointers
            let mut root_slots: Vec<*mut *mut u8> = roots
                .iter()
                .map(|r| r.stack_slot_addr as *mut *mut u8)
                .collect();

            // Append Rust-registered roots (from apply_cont_heap k2_stack, etc.)
            ms.extend_rust_roots(&mut root_slots);

            // Append session-scoped persistent roots (Wave 1.A, component D).
            // These survive across runs and are cleared only at machine drop.
            ms.extend_persistent_roots(&mut root_slots);

            // Append stowed roots (segment 40): the parent's suspended
            // continuation cell(s), registered for the duration of a nested
            // child run so this (child-triggered) collection evacuates the
            // parent's stowed continuation tree and rewrites the cell in
            // place. Empty in the non-nested case — a plain run/resume never
            // registers one, so this is a no-op there.
            ms.extend_stowed_roots(&mut root_slots);

            // Append write-barrier remembered slots: every recorded
            // old/external-to-young store (`write_barrier`), most notably a
            // tenured array's payload slot mutated by a later
            // `writeSmallArray#`/`WriteArray`/`casSmallArray#`/copy. Folded in
            // here so BOTH the first Cheney pass below and the doubling
            // re-evacuate (which reuses this same `root_slots` vector) trace
            // and rewrite them.
            ms.extend_remembered_slots(&mut root_slots);

            // Defense-in-depth: trace VMContext tail_callee/tail_arg
            // SAFETY: vmctx is valid and these fields are heap pointers.
            unsafe {
                let tc = &mut (*vmctx).tail_callee as *mut *mut u8;
                let ta = &mut (*vmctx).tail_arg as *mut *mut u8;
                if !(*tc).is_null() {
                    root_slots.push(tc);
                }
                if !(*ta).is_null() {
                    root_slots.push(ta);
                }
            }

            // Test-only one-shot fault injection (see `arm_gc_fault`): this
            // is exactly the window that used to hold a live `RefMut<GcState>`.
            maybe_raise_gc_fault(GcFaultPoint::DuringCopy);

            // SAFETY: root_slots point to valid stack locations from walk_frames.
            // from_start..from_end is the active nursery region. tospace is freshly
            // allocated with the same size, which always suffices: live data is a
            // subset of from-space and objects are copied at identical sizes.
            let result = unsafe {
                tidepool_heap::gc::raw::cheney_copy(
                    &root_slots,
                    from_start as *const u8,
                    from_end as *const u8,
                    as_bytes_mut(&mut tospace),
                )
            };

            maybe_raise_gc_fault(GcFaultPoint::AfterCopy);

            // Heap growth: a fixed-size heap turns large live sets into
            // premature OOM after GC thrash. When utilization is high,
            // immediately re-evacuate into a doubled space. The root slot
            // ADDRESSES collected above remain valid; their values now
            // point into `tospace`, so a second Cheney pass with
            // from = tospace relocates everything and re-updates them.
            let max_heap = max_heap_bytes();
            let mut active = tospace;
            let mut live_bytes = result.bytes_copied;
            let mut new_size = from_size;
            // Every range this collection evacuates OUT of, for the post-GC
            // verifier: the original nursery, plus (if the doubling branch
            // below runs) the intermediate to-space it evacuates a second
            // time.
            let mut retired_ranges: Vec<(*const u8, *const u8)> =
                vec![(from_start as *const u8, from_end as *const u8)];
            if live_bytes * 4 > from_size * 3 && from_size < max_heap {
                new_size = (from_size * 2).min(max_heap);
                let mut bigger = alloc_aligned_zeroed(new_size);
                // SAFETY: same contract as above; from-space is the live
                // prefix of `active`, disjoint from `bigger`.
                let second = unsafe {
                    let active_start = active.as_ptr() as *const u8;
                    tidepool_heap::gc::raw::cheney_copy(
                        &root_slots,
                        active_start,
                        active_start.add(live_bytes),
                        as_bytes_mut(&mut bigger),
                    )
                };
                live_bytes = second.bytes_copied;
                GC_DOUBLING_RUNS.fetch_add(1, Ordering::Relaxed);
                // Capture the intermediate to-space's full allocated range
                // BEFORE `active = bigger` drops it below: for this second
                // Cheney pass it was itself a from-space, so a pointer left
                // dangling into it is exactly as much a dangling evacuation
                // as one into the original nursery, and the verifier needs
                // both ranges to catch it.
                let intermediate_start = active.as_ptr() as *const u8;
                // SAFETY: `active` is a `Vec<u64>` of `active.len()` words;
                // the byte range it backs is valid for reads for its full
                // length.
                let intermediate_end = unsafe { intermediate_start.add(active.len() * 8) };
                retired_ranges.push((intermediate_start, intermediate_end));
                if gc_poison_enabled() {
                    // The intermediate to-space is a second (now-retired)
                    // from-space; poison it so anything left dangling into
                    // it fails loudly (see `gc_poison_enabled`).
                    active.iter_mut().for_each(|w| *w = 0xDDDD_DDDD_DDDD_DDDD);
                }
                active = bigger; // drops the intermediate tospace
            }

            if gc_poison_enabled() {
                // SAFETY: from_start..from_size is the pre-collection
                // nursery — still allocated here (the buffer is freed only
                // when `state.active_buffer` is replaced below, or is the
                // machine-owned initial nursery). All live data has been
                // evacuated; any pointer still aimed here is a GC bug this
                // poison makes deterministic.
                unsafe { std::ptr::write_bytes(from_start, 0xDD, from_size) };
            }

            // Update the owned GcState: swap to the surviving space.
            let to_start = active.as_mut_ptr() as *mut u8;
            state.active_start = to_start;
            state.active_size = new_size;
            state.active_buffer = Some(active); // drops old buffer if any

            // Put the state back BEFORE the post-GC verifier: the verifier
            // panics by design on an invariant violation, and that unwind
            // must not skip the restore the way a signal-triggered
            // siglongjmp would.
            ms.put_gc_state(state);

            // SAFETY: vmctx is a valid pointer passed from JIT code. to_start points
            // to the new active buffer which is now the nursery.
            unsafe {
                (*vmctx).alloc_ptr = to_start.add(live_bytes);
                (*vmctx).alloc_limit = to_start.add(new_size) as *const u8;
            }

            // Fail-loud heap invariant walk (TIDEPOOL_HEAP_VERIFY=1).
            // Runs while every retired range is still distinguishable, so a
            // surviving from-space pointer — a dangling evacuation, into
            // the original nursery OR (on the doubling path) the
            // intermediate to-space — is detected HERE, not three
            // collections later as a SIGSEGV.
            if heap_verify_enabled() {
                // SAFETY: to_start..+live_bytes is the packed live set
                // cheney_copy just produced; retired_ranges covers every
                // space this collection evacuated out of (addresses are
                // only compared, never dereferenced — see
                // `verify_heap_post_gc`'s doc).
                unsafe {
                    verify_heap_post_gc(to_start, live_bytes, &retired_ranges);
                }

                // The to-space walk above cannot reach old-space or a boxed
                // array's external payload buffer. These two cover that
                // population: the tenured-graph traversal catches an
                // old-to-young store the write barrier never recorded
                // (independent of the barrier, so it sees the barrier's own
                // misses), and the remembered-slot check confirms the
                // collector correctly rewrote every slot it WAS handed.
                let to_end = unsafe { to_start.add(live_bytes) as *const u8 };
                let mut persistent: Vec<*mut *mut u8> = Vec::new();
                ms.extend_persistent_roots(&mut persistent);
                let arenas = ms.old_space_arena_ranges();
                // SAFETY: persistent roots and arena ranges are this
                // machine's own registrations; retired ranges are compared,
                // never dereferenced.
                unsafe {
                    verify_tenured_graph(
                        &persistent,
                        &arenas,
                        to_start as *const u8,
                        to_end,
                        &retired_ranges,
                    );
                    verify_remembered_slots(
                        &ms.remembered_slots_snapshot(),
                        to_start as *const u8,
                        to_end,
                        &retired_ranges,
                    );
                }
            }
        }
        // ── End GC ─────────────────────────────────────────
        let _ = roots; // roots consumed by cheney_copy; explicit drop for clarity
    }
}

// Test instrumentation — NOT part of the public API.
// These use atomics to be thread-safe during parallel test execution.
static GC_TRIGGER_CALL_COUNT: AtomicU64 = AtomicU64::new(0);
static GC_TRIGGER_LAST_VMCTX: AtomicUsize = AtomicUsize::new(0);

/// A point inside `perform_gc`'s Cheney-copy body where a one-shot fault can
/// be injected via [`arm_gc_fault`]. Test instrumentation — NOT part of the
/// public API.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GcFaultPoint {
    None,
    /// After the `GcState` is taken out of its cell and to-space is
    /// allocated, immediately before `cheney_copy` — the window that used to
    /// hold a live `RefMut<GcState>`.
    DuringCopy,
    /// After `cheney_copy` returns and before the `GcState` is put back.
    AfterCopy,
}

impl GcFaultPoint {
    fn to_u8(self) -> u8 {
        match self {
            GcFaultPoint::None => 0,
            GcFaultPoint::DuringCopy => 1,
            GcFaultPoint::AfterCopy => 2,
        }
    }
}

/// Process-global one-shot GC fault arm point. Test instrumentation — NOT
/// part of the public API.
static GC_FAULT_POINT: AtomicU8 = AtomicU8::new(0);

/// Test-only: arm a one-shot fault at `point`. The next `perform_gc` pass
/// that reaches `point` disarms (stores `None`) before raising `SIGILL`, so
/// one `arm_gc_fault` call fires exactly one fault. Not part of the public
/// API.
#[doc(hidden)]
pub fn arm_gc_fault(point: GcFaultPoint) {
    GC_FAULT_POINT.store(point.to_u8(), Ordering::SeqCst);
}

/// Check-and-fire the fault armed for `point`, if any. `SIGILL` via
/// `libc::raise` is delivered synchronously to the calling thread before
/// `raise` returns and involves no UB, unlike an inline trap instruction
/// whose undefined behavior the optimizer is free to exploit.
///
/// The disarmed case costs one relaxed load — every collection runs this, so
/// the locked read-modify-write stays behind the armed check.
fn maybe_raise_gc_fault(point: GcFaultPoint) {
    if point == GcFaultPoint::None || GC_FAULT_POINT.load(Ordering::Relaxed) == 0 {
        return;
    }
    let armed = GC_FAULT_POINT.compare_exchange(
        point.to_u8(),
        GcFaultPoint::None.to_u8(),
        Ordering::SeqCst,
        Ordering::SeqCst,
    );
    if armed.is_ok() {
        unsafe { libc::raise(libc::SIGILL) };
    }
}

/// Reset test counters. Only call from tests.
pub fn reset_test_counters() {
    GC_TRIGGER_CALL_COUNT.store(0, Ordering::SeqCst);
    GC_TRIGGER_LAST_VMCTX.store(0, Ordering::SeqCst);
}

/// Get gc_trigger call count. Only call from tests.
pub fn gc_trigger_call_count() -> u64 {
    GC_TRIGGER_CALL_COUNT.load(Ordering::SeqCst)
}

/// Get last vmctx passed to gc_trigger. Only call from tests.
pub fn gc_trigger_last_vmctx() -> usize {
    GC_TRIGGER_LAST_VMCTX.load(Ordering::SeqCst)
}

/// Nursery allocation from host code with one GC-and-retry. Any heap
/// pointers the CALLER holds across this call must be RUST_ROOTS-registered.
///
/// # Safety
/// `vmctx` must be valid with a live nursery and GC state installed.
pub(crate) unsafe fn host_alloc_gc(vmctx: *mut VMContext, size: usize) -> *mut u8 {
    crate::heap_bridge::gc_retry(
        vmctx,
        |p: &*mut u8| p.is_null(),
        || crate::heap_bridge::bump_alloc_from_vmctx(&mut *vmctx, size),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout;

    /// The verifier must FIRE on a corrupted heap (size-wrap Con, the S3-C2
    /// shape) and stay SILENT on a healthy one.
    #[test]
    fn test_heap_verifier_fires_and_passes() {
        // Healthy to-space: one Lit(Int) + one 1-field Con pointing at it.
        // u64 backing => 8-aligned base (object starts must be 8-aligned).
        let mut buf = vec![0u64; 8];
        let base = buf.as_mut_ptr() as *mut u8;
        unsafe {
            // Lit at offset 0: tag=3, size=24, lit_tag=0 (Int), value=42.
            *base = layout::TAG_LIT;
            std::ptr::write_unaligned(base.add(1) as *mut u32, 24);
            *base.add(layout::LIT_TAG_OFFSET as usize) = 0;
            *(base.add(layout::LIT_VALUE_OFFSET as usize) as *mut i64) = 42;
            // Con at offset 24: tag=2, size=32, con_tag, num_fields=1, field -> Lit.
            let con = base.add(24);
            *con = layout::TAG_CON;
            std::ptr::write_unaligned(con.add(1) as *mut u32, 32);
            *(con.add(layout::CON_TAG_OFFSET as usize) as *mut u64) = 7;
            *(con.add(layout::CON_NUM_FIELDS_OFFSET as usize) as *mut u16) = 1;
            *(con.add(layout::CON_FIELDS_OFFSET as usize) as *mut *mut u8) = base;
            // from-space: an unrelated range that contains nothing we point at.
            let fake_from = 0x1000 as *const u8;
            let fake_from_end = 0x2000 as *const u8;
            let retired = [(fake_from, fake_from_end)];
            verify_heap_post_gc(base, 56, &retired); // silent

            // Corruption 1 (S3-C2 shape): num_fields says 4 but size says 32.
            *(con.add(layout::CON_NUM_FIELDS_OFFSET as usize) as *mut u16) = 4;
            let r = std::panic::catch_unwind(|| verify_heap_post_gc(base, 56, &retired));
            assert!(
                r.is_err(),
                "verifier must fire on Con size/num_fields mismatch"
            );
            *(con.add(layout::CON_NUM_FIELDS_OFFSET as usize) as *mut u16) = 1;

            // Corruption 2: dangling evacuation — field points into from-space.
            *(con.add(layout::CON_FIELDS_OFFSET as usize) as *mut *mut u8) = 0x1800 as *mut u8;
            let r = std::panic::catch_unwind(|| verify_heap_post_gc(base, 56, &retired));
            assert!(r.is_err(), "verifier must fire on from-space pointer");
            *(con.add(layout::CON_FIELDS_OFFSET as usize) as *mut *mut u8) = base;

            // Corruption 3: unknown lit tag (constant-drift class).
            *base.add(layout::LIT_TAG_OFFSET as usize) = 99;
            let r = std::panic::catch_unwind(|| verify_heap_post_gc(base, 56, &retired));
            assert!(r.is_err(), "verifier must fire on unknown lit tag");
        }
    }

    /// L8 (repo-review-2026-07-06/01-gc-memory-safety.md, Low findings):
    /// `alloc_aligned_zeroed`'s buffer must be 8-byte aligned regardless of
    /// size (including a size that ISN'T already a multiple of 8 — the
    /// rounding-up path), and `as_bytes_mut`'s byte view must cover the
    /// full requested length, zeroed.
    #[test]
    fn alloc_aligned_zeroed_is_always_8_aligned() {
        for size in [0usize, 1, 7, 8, 9, 63, 64, 65, 4096, 4099] {
            let mut words = alloc_aligned_zeroed(size);
            let ptr = words.as_mut_ptr() as usize;
            assert_eq!(
                ptr % 8,
                0,
                "size {size}: buffer base must be 8-aligned, got {ptr:#x}"
            );
            let bytes = as_bytes_mut(&mut words);
            assert!(
                bytes.len() >= size,
                "size {size}: byte view ({} bytes) must cover the requested size",
                bytes.len()
            );
            assert!(bytes.iter().all(|&b| b == 0), "size {size}: must be zeroed");
        }
    }
}
