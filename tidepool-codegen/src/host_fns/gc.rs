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
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

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

/// Per-machine state for the copying garbage collector.
pub(crate) struct GcState {
    pub active_start: *mut u8,
    pub active_size: usize,
    pub active_buffer: Option<Vec<u8>>,
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

/// Process-global test override for `heap_verify_enabled`. `env::set_var` is
/// racy against the `OnceLock`-cached env read below (it latches the FIRST
/// read) and unsafe on edition 2024; this atomic gives tests a reliable,
/// safe way to force the verifier on without touching the environment.
static HEAP_VERIFY_FORCE: AtomicBool = AtomicBool::new(false);

/// Test-only: force the post-GC heap verifier on (or back off), independent
/// of `TIDEPOOL_HEAP_VERIFY`. Not part of the public API.
#[doc(hidden)]
pub fn set_heap_verify(on: bool) {
    HEAP_VERIFY_FORCE.store(on, Ordering::Relaxed);
}

/// Kill-switched fail-loud mode: `TIDEPOOL_HEAP_VERIFY=1` (or `set_heap_verify`)
/// walks the entire live set after every GC and panics on the first invariant
/// violation. Tests opt in; production pays one cached env read.
fn heap_verify_enabled() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("TIDEPOOL_HEAP_VERIFY").is_ok_and(|v| v == "1"))
        || HEAP_VERIFY_FORCE.load(Ordering::Relaxed)
}

/// Count of completed `verify_heap_post_gc` runs, process-wide. Lets a test
/// prove the verifier actually fired rather than silently no-op'ing.
static HEAP_VERIFY_RUNS: AtomicUsize = AtomicUsize::new(0);

/// Test-only: how many times `verify_heap_post_gc` has run in this process.
#[doc(hidden)]
pub fn heap_verify_run_count() -> usize {
    HEAP_VERIFY_RUNS.load(Ordering::Relaxed)
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
/// - every pointer field is null (legal mid-LetRec-construction), inside
///   to-space (then 8-aligned), or outside BOTH spaces (poison / malloc'd
///   byte arrays) — a pointer into FROM-SPACE is a dangling evacuation and
///   fails loudly here instead of as a SIGSEGV collections later. BLACKHOLE
///   capture slots are checked too: `for_each_pointer_field` skips them
///   (S3-C6), so a from-space capture in a blackholed thunk is that bug
///   manifesting.
///
/// From-space addresses are COMPARED, never dereferenced (the buffer may
/// already be freed). Known v1 gap: in the heap-doubling path the
/// intermediate to-space is a second (untracked) from-space; pointers
/// dangling into it land in the "outside both" class and pass.
unsafe fn verify_heap_post_gc(
    to_start: *const u8,
    live_bytes: usize,
    from_start: *const u8,
    from_end: *const u8,
) {
    HEAP_VERIFY_RUNS.fetch_add(1, Ordering::Relaxed);
    use crate::layout as l;
    let to_end = to_start.add(live_bytes);
    let in_to = |p: *const u8| p >= to_start && p < to_end;
    let in_from = |p: *const u8| p >= from_start && p < from_end;

    let fail = |off: usize, idx: usize, what: &str, obj: *const u8| -> ! {
        let dump_len = 32.min(live_bytes - off);
        let bytes = std::slice::from_raw_parts(obj, dump_len);
        panic!(
            "[HEAP VERIFY] violation after GC: {what}\n  object #{idx} at to-space offset {off:#x} \
             (live_bytes={live_bytes:#x})\n  first {dump_len} bytes: {bytes:02x?}\n  \
             from-space was {from_start:p}..{from_end:p}, to-space {to_start:p}..{to_end:p}"
        )
    };

    let check_field = |off: usize, idx: usize, obj: *const u8, slot: usize, label: &str| {
        let p = *(obj.add(slot) as *const *const u8);
        if p.is_null() {
            return; // legal: deferred Con field mid-LetRec construction
        }
        if in_from(p) {
            fail(
                off,
                idx,
                &format!(
                    "{label} slot +{slot} holds a FROM-SPACE pointer {p:p} (dangling evacuation)"
                ),
                obj,
            );
        }
        if in_to(p) && !(p as usize).is_multiple_of(8) {
            fail(
                off,
                idx,
                &format!("{label} slot +{slot} holds a misaligned to-space pointer {p:p}"),
                obj,
            );
        }
        // Outside both spaces: poison object or malloc'd byte array — allowed.
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
                            if p.is_null() {
                                continue;
                            }
                            if in_from(p) {
                                fail(
                                    off,
                                    idx,
                                    &format!(
                                        "array elem[{i}] holds a FROM-SPACE pointer {p:p} (dangling evacuation)"
                                    ),
                                    obj,
                                );
                            }
                            if in_to(p) && !(p as usize).is_multiple_of(8) {
                                fail(
                                    off,
                                    idx,
                                    &format!(
                                        "array elem[{i}] holds a misaligned to-space pointer {p:p}"
                                    ),
                                    obj,
                                );
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
                let code = *(obj.add(l::CLOSURE_CODE_PTR_OFFSET as usize) as *const *const u8);
                if code.is_null() {
                    fail(off, idx, "Closure with null code pointer", obj);
                }
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
                        // for_each_pointer_field skips blackhole captures
                        // (S3-C6): a from-space capture here is that bug live.
                        let n = (size - l::THUNK_CAPTURED_OFFSET as usize) / 8;
                        for i in 0..n {
                            check_field(
                                off,
                                idx,
                                obj,
                                l::THUNK_CAPTURED_OFFSET as usize + 8 * i,
                                "BLACKHOLE capture (S3-C6: invisible to GC)",
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
        // SAFETY: fp is a valid frame pointer read from gc_trigger's caller.
        // registry contains stack maps for all JIT functions in the call chain.
        let roots = unsafe { frame_walker::walk_frames(fp, registry) };

        // ── Cheney copying GC ──────────────────────────────
        // SAFETY: vmctx is valid; machine_state was installed before entering
        // JIT code (same contract as the stack_map_registry read above).
        let ms = unsafe { machine_state(vmctx) };
        {
            let mut gc_state = ms.gc_state_mut();
            if let Some(state) = gc_state.as_mut() {
                // M3 (deep_force): a real collection is about to run — bump
                // so callers holding an address-keyed cache across this call
                // (e.g. deep_force's visited set) know to invalidate it.
                ms.bump_gc_generation();
                let from_start = state.active_start;
                let from_size = state.active_size;
                // SAFETY: from_start + from_size stays within the active GC region.
                let from_end = unsafe { from_start.add(from_size) };

                let mut tospace = vec![0u8; from_size];

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

                // SAFETY: root_slots point to valid stack locations from walk_frames.
                // from_start..from_end is the active nursery region. tospace is freshly
                // allocated with the same size, which always suffices: live data is a
                // subset of from-space and objects are copied at identical sizes.
                let result = unsafe {
                    tidepool_heap::gc::raw::cheney_copy(
                        &root_slots,
                        from_start as *const u8,
                        from_end as *const u8,
                        &mut tospace,
                    )
                };

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
                if live_bytes * 4 > from_size * 3 && from_size < max_heap {
                    new_size = (from_size * 2).min(max_heap);
                    let mut bigger = vec![0u8; new_size];
                    // SAFETY: same contract as above; from-space is the live
                    // prefix of `active`, disjoint from `bigger`.
                    let second = unsafe {
                        tidepool_heap::gc::raw::cheney_copy(
                            &root_slots,
                            active.as_ptr(),
                            active.as_ptr().add(live_bytes),
                            &mut bigger,
                        )
                    };
                    live_bytes = second.bytes_copied;
                    active = bigger; // drops the intermediate tospace
                }

                // Update GcState: swap to the surviving space
                let to_start = active.as_mut_ptr();
                state.active_start = to_start;
                state.active_size = new_size;
                state.active_buffer = Some(active); // drops old buffer if any

                // SAFETY: vmctx is a valid pointer passed from JIT code. to_start points
                // to the new active buffer which is now the nursery.
                unsafe {
                    (*vmctx).alloc_ptr = to_start.add(live_bytes);
                    (*vmctx).alloc_limit = to_start.add(new_size) as *const u8;
                }

                // Fail-loud heap invariant walk (TIDEPOOL_HEAP_VERIFY=1).
                // Runs while from-space is still distinguishable, so a
                // surviving from-space pointer — a dangling evacuation —
                // is detected HERE, not three collections later as a
                // SIGSEGV. (plans/future-plans.md item D)
                if heap_verify_enabled() {
                    // SAFETY: to_start..+live_bytes is the packed live set
                    // cheney_copy just produced; from range was the
                    // pre-collection nursery (old buffer still alive in
                    // state.active_buffer's predecessor scope).
                    unsafe {
                        verify_heap_post_gc(
                            to_start,
                            live_bytes,
                            from_start as *const u8,
                            from_end as *const u8,
                        );
                    }
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
    let p = crate::heap_bridge::bump_alloc_from_vmctx(&mut *vmctx, size);
    if !p.is_null() {
        return p;
    }
    gc_trigger(vmctx);
    crate::heap_bridge::bump_alloc_from_vmctx(&mut *vmctx, size)
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
            verify_heap_post_gc(base, 56, fake_from, fake_from_end); // silent

            // Corruption 1 (S3-C2 shape): num_fields says 4 but size says 32.
            *(con.add(layout::CON_NUM_FIELDS_OFFSET as usize) as *mut u16) = 4;
            let r = std::panic::catch_unwind(|| {
                verify_heap_post_gc(base, 56, fake_from, fake_from_end)
            });
            assert!(
                r.is_err(),
                "verifier must fire on Con size/num_fields mismatch"
            );
            *(con.add(layout::CON_NUM_FIELDS_OFFSET as usize) as *mut u16) = 1;

            // Corruption 2: dangling evacuation — field points into from-space.
            *(con.add(layout::CON_FIELDS_OFFSET as usize) as *mut *mut u8) = 0x1800 as *mut u8;
            let r = std::panic::catch_unwind(|| {
                verify_heap_post_gc(base, 56, fake_from, fake_from_end)
            });
            assert!(r.is_err(), "verifier must fire on from-space pointer");
            *(con.add(layout::CON_FIELDS_OFFSET as usize) as *mut *mut u8) = base;

            // Corruption 3: unknown lit tag (constant-drift class).
            *base.add(layout::LIT_TAG_OFFSET as usize) = 99;
            let r = std::panic::catch_unwind(|| {
                verify_heap_post_gc(base, 56, fake_from, fake_from_end)
            });
            assert!(r.is_err(), "verifier must fire on unknown lit tag");
        }
    }
}
