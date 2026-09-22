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
//! `(*vmctx).machine_state` is null; this permits hand-built test contexts
//! that do not install a machine.
//!
//! Isolation invariant: per-machine (not process-global) GC state is what
//! lets `MAX_CONCURRENT_EVALS` machines run concurrently on separate threads
//! without one eval's collector walking (and relocating objects in) another
//! eval's heap. A process-global GC pointer is forbidden in this module.

use crate::context::VMContext;
use crate::gc::frame_walker;
use crate::machine_state::{machine_state, machine_state_opt, MachineState};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

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

/// Current generation mark of the Rust-root stack. Pair with
/// `truncate_rust_roots` to
/// scope registrations: host fns that call back into JIT code can nest (e.g.
/// `heap_force` → thunk code → `heap_force`), so unscoped clearing would drop
/// an outer frame's registrations. Returns 0 when there is no machine to read.
///
/// # Safety
/// If `vmctx` is non-null, it must point to a live `VMContext`.
pub unsafe fn rust_roots_mark(vmctx: *mut VMContext) -> usize {
    machine_state_opt(vmctx)
        .map(|ms| ms.rust_roots_mark())
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

/// Register a SESSION-SCOPED GC root slot on the machine `vmctx` belongs to.
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
/// owning `PreparedMachine` drops). A slot freed or moved before that is a
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

/// Process-global test override: makes the remembered set refuse every new
/// slot -- from the write barrier AND from tenure-time payload remembering
/// (`MachineState::retain_external_payloads`) -- regardless of the machine's
/// armed state. Default off. This is the mutation-check kill switch
/// (#[doc(hidden)], test-only): flipping it on and re-running a test that
/// depends on an old-to-young edge being remembered must reproduce the SAME
/// pre-fix corruption signature, or the test proves nothing. It sits at the
/// one sink both recording paths share, so it cannot be dodged by whichever
/// path happens to cover a given scenario.
static REMEMBERED_SET_DISABLED_FOR_TEST: AtomicBool = AtomicBool::new(false);

/// Test-only: disable (or re-enable) remembered-set recording process-wide.
/// Not part of the public API.
#[doc(hidden)]
pub fn set_remembered_set_disabled_for_test(on: bool) {
    REMEMBERED_SET_DISABLED_FOR_TEST.store(on, Ordering::Relaxed);
}

pub(crate) fn remembered_set_disabled_for_test() -> bool {
    REMEMBERED_SET_DISABLED_FOR_TEST.load(Ordering::Relaxed)
}

/// THE write barrier for old-space OBJECT FIELDS (see `old_space.rs`'s module
/// doc, "Old-to-young edges"): every store of a possibly-young pointer into a
/// pointer field of an already-tenured object routes through this ONE
/// function -- thunk memoization/indirection cells (`OldSpace::tenure`, the
/// prepared enter's update) and `deep_force`'s constructor field rewrites.
/// `slot` is the ADDRESS of the pointer-sized location that was just written
/// -- NOT the value stored there. Records `slot` in the machine's remembered
/// set so `perform_gc` traces and rewrites it on every collection, exactly
/// like a stack or persistent root.
///
/// Boxed-array payload slots are NOT this barrier's job: a tenured array's
/// external payload has every slot remembered once, at tenure
/// (`MachineState::retain_external_payloads`), and a nursery array's payload
/// is discovered by the collection's own reachability expansion
/// (`trace_heap_region` -> `external_payload_view`). Array write sites
/// therefore call no barrier.
///
/// Cheap when unarmed: a relaxed load and return, before any hashing or
/// `RefCell` borrow — same shape as `maybe_raise_gc_fault`'s disarmed check.
/// Sound to skip while unarmed: before the first `OldSpace::tenure` call
/// there is no old-space, so no old-to-young store is possible yet.
///
/// `extern "C"` so generated code can call it directly (the prepared enter's
/// thunk update does); Rust call sites (`OldSpace::tenure`, `deep_force`)
/// call the exact same function. No-op when `vmctx`/`vmctx.machine_state` is
/// null (see the module doc) -- same null-vmctx invariant as
/// `register_persistent_root`.
///
/// # Safety
/// `slot` must be non-null and point to a valid, dereferenceable
/// `*mut u8`-sized location that stays valid until the memory holding it dies
/// (arena teardown / machine drop forgets it — see `forget_remembered_range`).
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn write_barrier(vmctx: *mut VMContext, slot: *mut *mut u8) {
    // SAFETY: vmctx is valid; machine_state was installed before entering
    // JIT code (same contract as every other vmctx-reached GC-cluster fn).
    if let Some(ms) = unsafe { machine_state_opt(vmctx) } {
        if !ms.write_barrier_armed() {
            return;
        }
        // Nursery fields move with their owners and are traced by Cheney's
        // object walk. Remembering their addresses would retain stale slots
        // after collection. External array payloads and old-space fields are
        // stable and do need the barrier.
        if ms.gc_active_range().is_some_and(|(start, size)| {
            let address = slot as usize;
            address >= start as usize && address - (start as usize) < size
        }) {
            return;
        }
        ms.register_remembered_slot(slot);
    }
}

/// Mutate an existing heap pointer field and record its generational edge.
///
/// # Safety
/// `slot` must be a valid writable pointer field. If it is outside the nursery,
/// its address must remain valid until the owning allocation is retired.
#[cfg(test)]
pub(super) unsafe fn store_heap_pointer(vmctx: *mut VMContext, slot: *mut *mut u8, value: *mut u8) {
    unsafe { *slot = value };
    write_barrier(vmctx, slot);
}

/// Per-machine state for the copying garbage collector.
pub(crate) struct GcState {
    pub active_start: *mut u8,
    pub active_size: usize,
    /// `Vec<u64>`, not `Vec<u8>` — see `SessionState::heap`'s doc
    /// (machine.rs) for why.
    pub active_buffer: Option<Vec<u64>>,
    pub prepared: Option<PreparedHeap>,
}

/// Physical metadata for the active prepared nursery, not a reachability set.
pub(crate) struct PreparedHeap {
    pub space: tidepool_heap::gc::raw::DescriptorSpace,
    /// Remains owned even when copying stops after moving only part of the heap.
    pub spare: Vec<u64>,
    pub used: usize,
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

static MAX_HEAP_OVERRIDE: AtomicUsize = AtomicUsize::new(0);

/// Test-only: force the heap growth ceiling to exactly `bytes`, independent
/// of `TIDEPOOL_MAX_HEAP`. Lets a test reach a genuine "cannot fit even after
/// growth" `HeapOverflow` without constructing a multi-gigabyte program. Not
/// part of the public API.
#[doc(hidden)]
pub fn set_max_heap_bytes_for_test(bytes: usize) {
    MAX_HEAP_OVERRIDE.store(bytes, Ordering::Relaxed);
}

/// Test-only: clear the heap-ceiling override and defer back to
/// `TIDEPOOL_MAX_HEAP`/the default. Not part of the public API.
#[doc(hidden)]
pub fn clear_max_heap_bytes_override() {
    MAX_HEAP_OVERRIDE.store(0, Ordering::Relaxed);
}

#[inline(never)]
/// Heap growth ceiling. Defaults to 1 GiB; override with `TIDEPOOL_MAX_HEAP`
/// (bytes), or with [`set_max_heap_bytes_for_test`] independent of the
/// environment. Reaching the cap with a full live set ends in a clean
/// `HeapOverflow` via the post-GC allocation re-check, never a signal.
fn max_heap_bytes() -> usize {
    use std::sync::OnceLock;
    let overridden = MAX_HEAP_OVERRIDE.load(Ordering::Relaxed);
    if overridden != 0 {
        return overridden;
    }
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

/// The one heap-growth policy, shared by the legacy (`perform_gc_request`)
/// and prepared (`collect_prepared`) collectors. After a copy that left
/// `live_bytes` in a `from_size`-byte space, decide the space's next size
/// given the allocation (`reserve` bytes) whose failure triggered the
/// collection:
///
/// - grow when utilization is high (`live*4 > from*3`, the thrash guard) OR
///   when `live + reserve` does not fit -- the pending request is part of the
///   decision, never a separate post-collection re-check that can only fail;
/// - the new size is at least double and at least `live + reserve`, capped at
///   `ceiling`;
/// - `Ok(None)` means no growth is needed; `Err(HeapOverflow)` means the
///   request cannot fit even at the ceiling.
///
/// Both collectors decide growth once per collection with this function, so
/// a live set exactly on the utilization boundary plus one object larger
/// than the free remainder cannot fall between two rules.
pub(crate) fn heap_growth_target(
    from_size: usize,
    live_bytes: usize,
    reserve: usize,
    ceiling: usize,
) -> Result<Option<usize>, crate::host_fns::RuntimeError> {
    let needed = live_bytes
        .checked_add(reserve)
        .ok_or(crate::host_fns::RuntimeError::HeapOverflow)?;
    if needed > ceiling {
        return Err(crate::host_fns::RuntimeError::HeapOverflow);
    }
    let high_utilization = live_bytes.saturating_mul(4) > from_size.saturating_mul(3);
    if (high_utilization || needed > from_size) && from_size < ceiling {
        return Ok(Some(from_size.saturating_mul(2).max(needed).min(ceiling)));
    }
    Ok(None)
}

fn collect_prepared(
    machine: &MachineState,
    state: &mut GcState,
    roots: &[*mut *mut u8],
    from_used: usize,
    reserve: usize,
    completed_copy: &mut bool,
    admitted: Option<&dyn tidepool_heap::descriptor_region::DescriptorOldSpace>,
) -> Result<usize, crate::host_fns::RuntimeError> {
    use crate::host_fns::RuntimeError;
    use tidepool_heap::execution_descriptor::DescriptorTraceError;
    let active = state
        .active_buffer
        .as_mut()
        .ok_or(RuntimeError::BadPointer)?;
    let prepared = state.prepared.as_mut().ok_or(RuntimeError::BadPointer)?;
    let ceiling = max_heap_bytes() & !7;
    if reserve > ceiling || ceiling < 8 {
        return Err(RuntimeError::HeapOverflow);
    }
    prepared.used = from_used;
    loop {
        let words = state.active_size.div_ceil(8);
        if prepared.spare.len() < words {
            prepared
                .spare
                .try_reserve_exact(words - prepared.spare.len())
                .map_err(|_| RuntimeError::HeapOverflow)?;
            prepared.spare.resize(words, 0);
        }
        // SAFETY: the owning heap and checked snapshot keep source objects and
        // root slots live; the new buffer is disjoint and fully initialized.
        let copied = unsafe {
            tidepool_heap::gc::raw::cheney_copy_descriptors_with_external(
                roots,
                state.active_start,
                prepared.used,
                as_bytes_mut(&mut prepared.spare),
                &mut prepared.space,
                admitted,
                machine,
            )
        };
        match copied {
            Ok(result) => {
                // No fallible work between copying and publishing ownership.
                std::mem::swap(active, &mut prepared.spare);
                *completed_copy = true;
                // `spare` is now the retired from-space. Under
                // `gc_poison_enabled()` it is filled with the same tag the
                // legacy collector uses, so a pointer the trace missed (an
                // unregistered frame, a forgotten root) reads back as an
                // unmistakable failure instead of the stale-but-plausible
                // object it used to point at. Nothing reads this buffer
                // again until the next copy overwrites it.
                if gc_poison_enabled() {
                    prepared.spare.fill(0xDDDD_DDDD_DDDD_DDDD);
                }
                state.active_start = active.as_mut_ptr().cast();
                state.active_size = std::mem::size_of_val(active.as_slice());
                prepared.used = result.bytes_copied;
                let Some(size) =
                    heap_growth_target(state.active_size, prepared.used, reserve, ceiling)?
                else {
                    // Only the final successful copy's marks decide Young
                    // payload liveness. Growth recopies reuse root-slot
                    // addresses, so sweeping between copies is forbidden.
                    sweep_prepared_young(machine, &prepared.space)?;
                    return Ok(prepared.used);
                };
                let words = size.div_ceil(8);
                prepared
                    .spare
                    .try_reserve_exact(words - prepared.spare.len())
                    .map_err(|_| RuntimeError::HeapOverflow)?;
                prepared.spare.resize(words, 0);
            }
            Err(DescriptorTraceError::MetadataAllocation) => {
                return Err(RuntimeError::HeapOverflow)
            }
            Err(_) => return Err(RuntimeError::BadPointer),
        }
    }
}

fn sweep_prepared_young(
    machine: &crate::machine_state::MachineState,
    space: &tidepool_heap::gc::raw::DescriptorSpace,
) -> Result<(), crate::host_fns::RuntimeError> {
    use crate::host_fns::RuntimeError;
    use tidepool_heap::external_storage::ExternalStorageValidationError;
    let mut marked = HashSet::new();
    let payloads = space.visited_external_payloads();
    marked
        .try_reserve(payloads.len())
        .map_err(|_| RuntimeError::HeapOverflow)?;
    marked.extend(payloads.map(|(address, _)| address as *mut u8));
    let classify = |error| match error {
        ExternalStorageValidationError::BookkeepingAllocation => RuntimeError::HeapOverflow,
        _ => RuntimeError::BadPointer,
    };
    let plan = machine
        .plan_external_minor_sweep(&marked)
        .map_err(classify)?;
    machine.commit_external_sweep(plan).map_err(classify)?;
    Ok(())
}

/// Prepared allocation safepoint. Failure is an explicit ABI status, never an
/// allocation-shaped poison pointer. The recorded first cause remains owned by
/// the machine for the run boundary to report.
#[inline(never)]
pub(crate) unsafe extern "C" fn prepared_gc_trigger(vmctx: *mut VMContext, reserve: usize) -> i32 {
    let mut frame_anchor = [0_u64; 2];
    std::hint::black_box(&mut frame_anchor);
    let ms = unsafe { machine_state(vmctx) };
    if ms.poll_prepared(crate::prepared_control::PreparedSafepoint::Allocation)
        == crate::prepared_control::CallStatus::Success
    {
        let state = ms.take_gc_state();
        let is_prepared = state.as_ref().is_some_and(|state| state.prepared.is_some());
        if let Some(state) = state {
            ms.put_gc_state(state);
        }
        if !is_prepared {
            ms.set_first_cause(crate::host_fns::RuntimeError::BadPointer);
            return ms.prepared_call_status() as i32;
        }
        #[cfg(target_arch = "x86_64")]
        {
            let fp: usize;
            unsafe {
                std::arch::asm!("mov {}, rbp", out(reg) fp, options(nomem, nostack));
            }
            perform_gc_request(fp, vmctx, reserve);
        }
        #[cfg(not(target_arch = "x86_64"))]
        ms.set_first_cause(crate::host_fns::RuntimeError::BadPointer);
    }
    ms.prepared_call_status() as i32
}

fn perform_gc_request(fp: usize, vmctx: *mut VMContext, reserve: usize) {
    // SAFETY: vmctx is valid; machine_state was installed before entering JIT code.
    let ms = unsafe { machine_state(vmctx) };
    let Some(registries) = ms.stack_map_chain() else {
        ms.set_first_cause(crate::host_fns::RuntimeError::IncompleteRootSnapshot(
            frame_walker::FrameWalkError::RegistryUnavailable,
        ));
        return;
    };
    // Every linked registry was set by set_stack_map_registry/
    // push_stack_map_registry and outlives JIT execution; the `registries`
    // snapshot is dropped as soon as the walk finishes.
    // `stack_low` is a local in THIS frame. perform_gc is always called
    // beneath the JIT call chain (gc_trigger → perform_gc, never the
    // reverse), and the stack grows down, so this address is a sound
    // LOW bound: every JIT frame `walk_frames` is about to walk sits at
    // a strictly higher address than this one.
    let stack_low: u8 = 0;
    let bounds = frame_walker::StackBounds::capture(&stack_low as *const u8 as usize);
    // SAFETY: fp is a valid frame pointer read from gc_trigger's caller.
    // The chain covers stack maps for every JIT pipeline installed on this
    // machine, resolved per frame through the machine's code-range index --
    // return addresses never collide across pipelines, so at most one
    // registry recognizes any given frame. A violation of that contract is
    // now a controlled failure, not UB -- see `walk_frames`'s doc.
    let roots = match unsafe {
        frame_walker::walk_frames(fp, &registries, bounds, heap_verify_enabled())
    } {
        Ok(roots) => roots,
        Err(error) => {
            ms.set_first_cause(crate::host_fns::RuntimeError::IncompleteRootSnapshot(error));
            return;
        }
    };
    drop(registries);

    // ── Cheney copying GC ──────────────────────────────
    // SAFETY: vmctx is valid; machine_state was installed before entering
    // JIT code (same contract as the stack_map_registry read above).
    // The `GcState` is taken out of its cell for the duration of the copy,
    // rather than keeping a `RefCell` borrow live through collection. The
    // empty-cell no-op also means a reentrant `perform_gc` call
    // (this function calling itself, transitively, while `state` is
    // taken) would silently skip its own collection instead of running
    // one. `OldSpace::tenure`'s `run_minor_collection_for_tenure_fixup`
    // call relies on that reentrancy never happening — but the no-op
    // is a BACKSTOP, not the correctness mechanism: the actual
    // guarantee is that nothing in this function's body (or its
    // transitive callees) ever calls `OldSpace::tenure` — there is no
    // such call today. If a future change adds one anywhere reachable
    // from here, verify it cannot run while `state` is taken before
    // relying on this no-op to make it safe; otherwise a nested tenure
    // fixup pass would silently no-op instead of fixing up siblings,
    // regressing into the exact bug this mechanism exists to close.
    if let Some(mut state) = ms.take_gc_state() {
        let from_start = state.active_start;
        let from_size = state.active_size;
        let alloc_ptr = unsafe { (*vmctx).alloc_ptr } as usize;
        let from_used = alloc_ptr.checked_sub(from_start as usize);
        let Some(from_used) = from_used.filter(|&used| used <= from_size) else {
            ms.put_gc_state(state);
            ms.set_first_cause(crate::host_fns::RuntimeError::BadPointer);
            return;
        };

        let stack_slots: Vec<*mut *mut u8> = roots
            .iter()
            .map(|root| root.stack_slot_addr as *mut *mut u8)
            .collect();
        let snapshot = unsafe { ms.complete_root_snapshot(&stack_slots) };
        let mut completed_copy = false;
        // The invocation-owned OldSpace is boxed and remains stable while
        // this collection runs. Its exact-start admission rejects stale
        // interiors and preexisting forwarded headers.
        let admitted = unsafe {
            ms.prepared_old_space()
                .map(|owner| owner as &dyn tidepool_heap::descriptor_region::DescriptorOldSpace)
        };
        let result = collect_prepared(
            ms,
            &mut state,
            &snapshot.into_slots(),
            from_used,
            reserve,
            &mut completed_copy,
            admitted,
        );
        // Even a later growth failure leaves the first completed copy
        // published. Never restore a cursor into the retired semispace.
        if completed_copy {
            ms.bump_gc_generation();
            if let Some(prepared) = state.prepared.as_ref() {
                unsafe {
                    (*vmctx).alloc_ptr = state.active_start.add(prepared.used);
                    (*vmctx).alloc_limit = state.active_start.add(state.active_size);
                }
            }
        }
        match result {
            Ok(used) => unsafe {
                (*vmctx).alloc_ptr = state.active_start.add(used);
                (*vmctx).alloc_limit = state.active_start.add(state.active_size);
            },
            Err(error) => ms.set_first_cause(error),
        }
        ms.put_gc_state(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_collection_reserves_capacity_and_rewrites_only_managed_fields() {
        use std::sync::Arc;
        use tidepool_heap::execution_descriptor::ObjectDescriptor;
        use tidepool_repr::execution_schema::{
            Architecture, Endianness, RuntimeRep, StorageLayout, TargetDescriptor,
        };
        let target = TargetDescriptor {
            architecture: Architecture::X86_64,
            endianness: Endianness::Little,
            pointer_width: 64,
            word_width: 64,
            abi: "sysv64".into(),
            features: Vec::new(),
        };
        let descriptor = Arc::new(
            ObjectDescriptor::constructor(
                1,
                StorageLayout::for_reps(&target, &[RuntimeRep::LiftedRef, RuntimeRep::Address])
                    .unwrap(),
                None,
            )
            .unwrap(),
        );
        let extent = descriptor.allocation_extent() as usize;
        let ms = crate::machine_state::MachineState::new();
        ms.install_prepared_buffer(vec![0_u64; extent / 8], vec![Arc::clone(&descriptor)])
            .unwrap();
        let (start, size) = ms.gc_active_range().unwrap();
        let mut vmctx = unsafe { VMContext::new(start, start.add(size)) };
        vmctx.machine_state = &ms as *const _ as *mut _;
        vmctx.alloc_ptr = unsafe { start.add(extent) };
        let managed_offset = descriptor.trace_offsets()[0] as usize;
        let address_offset =
            (descriptor.payload_base() + descriptor.payload().fields()[1].offset()) as usize;
        unsafe {
            descriptor.initialize_header(start);
            *start.add(managed_offset).cast::<*mut u8>() = start;
            *start.add(address_offset).cast::<usize>() = start as usize;
        }
        let mut root = start;
        ms.register_rust_root(&mut root);
        let maps = crate::stack_map::StackMapRegistry::new();
        ms.set_stack_map_registry(&maps);
        perform_gc_request(0, &mut vmctx, extent * 4);
        assert_eq!(
            ms.prepared_call_status(),
            crate::prepared_control::CallStatus::Success
        );
        assert_eq!(root, ms.gc_active_range().unwrap().0);
        assert!(vmctx.alloc_limit as usize - vmctx.alloc_ptr as usize >= extent * 4);
        unsafe {
            assert_eq!(*root.add(managed_offset).cast::<*mut u8>(), root);
            assert_eq!(*root.add(address_offset).cast::<usize>(), start as usize);
        }
        assert_eq!(ms.gc_generation(), 1);
        // Once both spaces have reached the active size, ordinary collections
        // reuse them. Only object addresses change; raw Address bits do not.
        perform_gc_request(0, &mut vmctx, 0);
        let first_space = root;
        perform_gc_request(0, &mut vmctx, 0);
        let second_space = root;
        assert_ne!(first_space, second_space);
        perform_gc_request(0, &mut vmctx, 0);
        assert_eq!(root, first_space);

        // A valid request whose live-plus-reserve exceeds the ceiling is
        // rejected after copying. The published cursor must describe that
        // completed copy, not the old source, and the heap stays reusable.
        let failed_young = ms
            .allocate_external_storage(
                tidepool_heap::external_storage::ExternalStorageKind::Bytes,
                0,
            )
            .unwrap();
        perform_gc_request(0, &mut vmctx, max_heap_bytes() & !7);
        assert_eq!(
            ms.prepared_call_status(),
            crate::prepared_control::CallStatus::LanguageFailure
        );
        assert_eq!(
            ms.disposition(),
            crate::machine_state::MachineDisposition::Reusable
        );
        let (active, _) = ms.gc_active_range().unwrap();
        assert_eq!(root, active);
        assert_eq!(vmctx.alloc_ptr, unsafe { active.add(extent) });
        unsafe {
            assert_eq!(*root.add(managed_offset).cast::<*mut u8>(), root);
            assert_eq!(*root.add(address_offset).cast::<usize>(), start as usize);
        }
        assert_eq!(ms.external_storage_stats().live_objects, 1);
        assert_eq!(ms.external_storage_stats().freed_objects, 0);
        assert_eq!(unsafe { failed_young.cast::<u64>().read() }, 0);
        ms.clear_gc_state();
        ms.clear_stack_map_registry();
    }

    #[test]
    fn prepared_collection_traces_owned_boxed_payload_through_growth() {
        use std::alloc::{alloc_zeroed, Layout};
        use std::sync::Arc;
        use tidepool_heap::execution_descriptor::{DescriptorState, ObjectDescriptor};
        use tidepool_heap::external_storage::ExternalStorageKind;
        use tidepool_heap::managed_reference::untag;
        use tidepool_repr::execution_schema::{
            Architecture, Endianness, StorageLayout, TargetDescriptor,
        };

        let target = TargetDescriptor {
            architecture: Architecture::X86_64,
            endianness: Endianness::Little,
            pointer_width: 64,
            word_width: 64,
            abi: "sysv64".into(),
            features: Vec::new(),
        };
        let wrapper =
            Arc::new(ObjectDescriptor::external(ExternalStorageKind::BoxedArray, &target).unwrap());
        let leaf = Arc::new(
            ObjectDescriptor::constructor(1, StorageLayout::for_reps(&target, &[]).unwrap(), None)
                .unwrap(),
        );
        let used = wrapper.allocation_extent() as usize + leaf.allocation_extent() as usize;
        let ms = crate::machine_state::MachineState::new();
        ms.install_prepared_buffer(vec![0_u64; used / 8], vec![wrapper.clone(), leaf.clone()])
            .unwrap();
        let (start, size) = ms.gc_active_range().unwrap();
        let child = unsafe { start.add(wrapper.allocation_extent() as usize) };
        unsafe {
            wrapper.initialize_header(start);
            leaf.initialize_header(child);
        }

        let layout = Layout::from_size_align(16, 8).unwrap();
        let payload = unsafe { alloc_zeroed(layout) };
        assert!(!payload.is_null());
        unsafe {
            payload.cast::<u64>().write(1);
            payload
                .add(8)
                .cast::<*mut u8>()
                .write((child as usize | usize::from(leaf.tag())) as *mut u8);
            wrapper
                .external_payload_slot(start, wrapper.allocation_extent() as usize)
                .unwrap()
                .write(payload);
        }
        ms.register_external_storage(
            payload,
            payload,
            layout,
            0,
            ExternalStorageKind::BoxedArray,
            1,
        );

        // An unreachable Young allocation is reclaimed after the complete
        // successful operation, including its growth recopy.
        ms.allocate_external_storage(ExternalStorageKind::Bytes, 0)
            .unwrap();

        let mut root = start;
        ms.register_rust_root(&mut root);
        let maps = crate::stack_map::StackMapRegistry::new();
        ms.set_stack_map_registry(&maps);
        let mut vmctx = unsafe { VMContext::new(start, start.add(size)) };
        vmctx.machine_state = &ms as *const _ as *mut _;
        vmctx.alloc_ptr = unsafe { start.add(used) };
        perform_gc_request(0, &mut vmctx, used * 4);

        assert_eq!(
            ms.prepared_call_status(),
            crate::prepared_control::CallStatus::Success
        );
        let (active, active_size) = ms.gc_active_range().unwrap();
        assert_eq!(root, active);
        assert!(
            active_size >= used * 5,
            "the copy must include growth reserve"
        );
        let moved_child = unsafe { payload.add(8).cast::<*mut u8>().read() } as usize;
        assert_eq!(
            untag(moved_child),
            active as usize + wrapper.allocation_extent() as usize
        );
        assert_eq!(
            unsafe {
                leaf.state(
                    untag(moved_child) as *const u8,
                    leaf.allocation_extent() as usize,
                )
            }
            .unwrap(),
            DescriptorState::Live
        );
        assert_eq!(
            unsafe {
                wrapper
                    .external_payload_slot(root, wrapper.allocation_extent() as usize)
                    .unwrap()
                    .read()
            },
            payload
        );
        assert_eq!(ms.external_storage_stats().live_objects, 1);
        assert_eq!(ms.external_storage_stats().freed_objects, 1);

        // Promotion retains the payload independently of its Young wrapper.
        // Its remembered slot still keeps the child valid when the wrapper
        // itself becomes unreachable in the next minor collection.
        ms.retain_external_payloads(&[(payload as usize, ExternalStorageKind::BoxedArray)])
            .unwrap();
        let retained_child = unsafe { payload.add(8).cast::<*mut u8>().read() };
        assert_eq!(
            ms.compare_exchange_external_element(payload, 0, retained_child, retained_child)
                .unwrap(),
            retained_child
        );
        ms.allocate_external_storage(ExternalStorageKind::Bytes, 0)
            .unwrap();
        root = std::ptr::null_mut();
        perform_gc_request(0, &mut vmctx, 0);
        assert_eq!(
            ms.prepared_call_status(),
            crate::prepared_control::CallStatus::Success
        );
        assert_eq!(ms.external_storage_stats().live_objects, 1);
        assert_eq!(ms.external_storage_stats().freed_objects, 2);
        let moved_child = unsafe { payload.add(8).cast::<*mut u8>().read() } as usize;
        assert_eq!(untag(moved_child), ms.gc_active_range().unwrap().0 as usize);
        assert_eq!(root, std::ptr::null_mut());
        ms.clear_gc_state();
        ms.clear_stack_map_registry();
    }

    #[test]
    fn prepared_capacity_overflow_preserves_heap_and_first_cause() {
        let ms = crate::machine_state::MachineState::new();
        ms.install_prepared_buffer(vec![0_u64; 8], Vec::new())
            .unwrap();
        let (start, size) = ms.gc_active_range().unwrap();
        let mut vmctx = unsafe { VMContext::new(start, start.add(size)) };
        vmctx.machine_state = &ms as *const _ as *mut _;
        let maps = crate::stack_map::StackMapRegistry::new();
        ms.set_stack_map_registry(&maps);
        perform_gc_request(0, &mut vmctx, usize::MAX);
        assert_eq!(ms.gc_active_range(), Some((start, size)));
        assert_eq!(vmctx.alloc_ptr, start);
        assert_eq!(ms.gc_generation(), 0);
        assert_eq!(
            ms.prepared_call_status(),
            crate::prepared_control::CallStatus::LanguageFailure
        );
        // Observing ABI status must not consume or replace the machine cause.
        assert_eq!(
            ms.take_runtime_error(),
            Some(crate::host_fns::RuntimeError::HeapOverflow)
        );
        ms.clear_gc_state();
        ms.clear_stack_map_registry();
    }

    #[test]
    fn prepared_status_keeps_integrity_failure_after_cause_is_reported() {
        use crate::host_fns::RuntimeError;
        use crate::prepared_control::CallStatus;
        let ms = crate::machine_state::MachineState::new();
        ms.set_first_cause(RuntimeError::Cancelled);
        assert_eq!(ms.prepared_call_status(), CallStatus::Cancelled);
        ms.set_first_cause(RuntimeError::BadPointer);
        assert_eq!(ms.prepared_call_status(), CallStatus::IntegrityFailure);
        assert_eq!(ms.take_runtime_error(), Some(RuntimeError::Cancelled));
        assert_eq!(ms.prepared_call_status(), CallStatus::IntegrityFailure);
    }

    #[test]
    fn missing_root_registry_aborts_before_collection_and_marks_unavailable() {
        let ms = crate::machine_state::MachineState::new();
        ms.install_prepared_buffer(vec![0_u64; 8], Vec::new())
            .unwrap();
        let (start, size) = ms.gc_active_range().unwrap();
        let mut vmctx = unsafe { VMContext::new(start, start.add(size)) };
        vmctx.alloc_ptr = unsafe { start.add(16) };
        vmctx.machine_state = &ms as *const _ as *mut _;
        let before_range = ms.gc_active_range();

        perform_gc_request(0, &mut vmctx, 0);

        assert_eq!(ms.gc_generation(), 0, "collection must not begin");
        assert_eq!(
            ms.gc_active_range(),
            before_range,
            "GC state must stay installed"
        );
        assert_eq!(vmctx.alloc_ptr, unsafe { start.add(16) });
        assert!(matches!(
            ms.take_runtime_error(),
            Some(crate::host_fns::RuntimeError::IncompleteRootSnapshot(
                frame_walker::FrameWalkError::RegistryUnavailable
            ))
        ));
        assert_eq!(
            ms.disposition(),
            crate::machine_state::MachineDisposition::Unavailable
        );
    }

    #[test]
    fn barrier_remembers_stable_fields_but_never_nursery_addresses() {
        let ms = crate::machine_state::MachineState::new();
        ms.install_prepared_buffer(vec![0_u64; 8], Vec::new())
            .unwrap();
        let (start, size) = ms.gc_active_range().unwrap();
        ms.arm_write_barrier();
        let mut vmctx = unsafe { VMContext::new(start, start.add(size)) };
        vmctx.machine_state = &ms as *const _ as *mut _;
        let mut external = std::ptr::null_mut();
        unsafe {
            store_heap_pointer(&mut vmctx, start as *mut *mut u8, start.add(32));
            store_heap_pointer(&mut vmctx, &mut external, start.add(32));
        }
        let remembered = ms.remembered_slots_snapshot();
        assert_eq!(remembered, vec![&mut external as *mut *mut u8]);
        assert_eq!(external, unsafe { start.add(32) });
    }
}
