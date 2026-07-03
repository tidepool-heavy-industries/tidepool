//! GC roots, the thread-local GC/nursery state, and the `gc_trigger` slow path
//! (frame walk + Cheney copy) called from JIT allocation sites.

use crate::context::VMContext;
use crate::gc::frame_walker;
use crate::stack_map::StackMapRegistry;
use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use super::cancel::check_cancel_and_set_error;

thread_local! {
    /// Registry of stack maps for JIT functions.
    /// This is set before calling into JIT code so gc_trigger can access it.
    static STACK_MAP_REGISTRY: RefCell<Option<*const StackMapRegistry>> = const { RefCell::new(None) };

    pub(crate) static GC_STATE: RefCell<Option<GcState>> = const { RefCell::new(None) };

    /// Heap pointer slots registered by Rust code (e.g., apply_cont_heap's k2_stack)
    /// so GC can update them in-place when objects move during collection.
    static RUST_ROOTS: RefCell<Vec<*mut *mut u8>> = const { RefCell::new(Vec::new()) };

    /// SESSION-SCOPED GC roots (Wave 1.A, component D). Parallel to `RUST_ROOTS`,
    /// but with a *session* lifetime: registered via [`register_persistent_root`]
    /// (typically a tenured binding's stable slot), appended to the root set on
    /// every `perform_gc`, and cleared ONLY at machine drop ([`free_session_heap`])
    /// — never by the per-run [`clear_run_scratch`].
    ///
    /// This is what lets a GHCi-style session's bound values survive across runs:
    /// a collection fired by run N+1 still traces (and rewrites in place) the
    /// slots holding run N's persisted heap pointers, so they neither leak nor
    /// dangle. Slots point into the machine-owned session heap (the migrated
    /// `active_buffer`) or into `old_space`, valid until the `JitEffectMachine`
    /// drops.
    static PERSISTENT_ROOTS: RefCell<Vec<*mut *mut u8>> = const { RefCell::new(Vec::new()) };
}

/// Register a Rust stack/heap slot containing a heap pointer as a GC root.
/// GC will update the slot's value in-place if the pointed-to object moves.
///
/// # Safety
/// The slot must remain valid and dereferenceable until the matching
/// `truncate_rust_roots` (or `clear_rust_roots`) call.
pub unsafe fn register_rust_root(slot: *mut *mut u8) {
    RUST_ROOTS.with(|r| r.borrow_mut().push(slot));
}

/// Current depth of the Rust-root stack. Pair with `truncate_rust_roots` to
/// scope registrations: host fns that call back into JIT code can nest (e.g.
/// `heap_force` → thunk code → `heap_force`), so unscoped clearing would drop
/// an outer frame's registrations.
pub fn rust_roots_mark() -> usize {
    RUST_ROOTS.with(|r| r.borrow().len())
}

/// Drop roots registered after `mark`, preserving outer registrations.
pub fn truncate_rust_roots(mark: usize) {
    RUST_ROOTS.with(|r| r.borrow_mut().truncate(mark));
}

/// Remove all registered Rust roots. Call after the GC-unsafe region ends.
pub fn clear_rust_roots() {
    RUST_ROOTS.with(|r| r.borrow_mut().clear());
}

/// Register a SESSION-SCOPED GC root slot (Wave 1.A, component D).
///
/// Unlike [`register_rust_root`] (run-scoped, cleared every `RegistryGuard`
/// drop), a persistent root survives across runs and is cleared only by
/// [`free_session_heap`] at machine drop. `perform_gc` appends these to the
/// root set on every collection, so the slot's stored pointer is kept live and
/// rewritten in place when the pointee moves.
///
/// # Safety
/// `slot` must be non-null, point to a valid `*mut u8` heap-pointer location,
/// and remain valid + dereferenceable until [`free_session_heap`] runs (the
/// owning `JitEffectMachine` drops). A slot freed or moved before that is a
/// use-after-free the GC will trip on.
pub unsafe fn register_persistent_root(slot: *mut *mut u8) {
    PERSISTENT_ROOTS.with(|r| r.borrow_mut().push(slot));
}

/// Number of registered persistent roots (test/diagnostic accessor).
pub fn persistent_roots_count() -> usize {
    PERSISTENT_ROOTS.with(|r| r.borrow().len())
}

/// Remove all registered persistent roots. Called by [`free_session_heap`] at
/// machine drop — NOT per run. After this the slots must not be dereferenced.
pub fn clear_persistent_roots() {
    PERSISTENT_ROOTS.with(|r| r.borrow_mut().clear());
}

/// The current active GC region as `(start, size_bytes)`, or `None` if no GC
/// state is installed on this thread.
///
/// After install this is the nursery (one-shot / session first run) or the
/// retained session heap (session re-entry); after a collection it is the
/// surviving `active_buffer`. Used by tenuring (the nursery from-range to
/// evacuate out of, component E) and by the Wave 1.A seam test (to assert
/// `install_registries` re-points at the retained heap rather than resetting to
/// `nursery.start()`).
pub fn gc_active_range() -> Option<(*mut u8, usize)> {
    GC_STATE.with(|cell| {
        cell.borrow()
            .as_ref()
            .map(|s| (s.active_start, s.active_size))
    })
}

/// Thread-local state for the copying garbage collector.
pub(crate) struct GcState {
    pub active_start: *mut u8,
    pub active_size: usize,
    pub active_buffer: Option<Vec<u8>>,
}

// SAFETY: GcState contains raw pointers but is only accessed from the thread that created it.
unsafe impl Send for GcState {}

/// Set the active GC state for the current thread.
pub fn set_gc_state(start: *mut u8, size: usize) {
    GC_STATE.with(|cell| {
        *cell.borrow_mut() = Some(GcState {
            active_start: start,
            active_size: size,
            active_buffer: None,
        });
    });
}

/// Clear the active GC state for the current thread.
///
/// LIFECYCLE SEAM (Wave 1.A, review item 2/4 — frozen here, body unchanged):
/// today this runs on every `RegistryGuard::drop` and both (a) drops `GcState`
/// — which, after the first GC, OWNS the live heap in `active_buffer`
/// (`host_fns.rs` `perform_gc`) — and (b) wipes all roots. For a persistent
/// session that is a use-after-free: a GC between two fragments would free the
/// heap and strand every persisted pointer. Wave 1.A splits this into the two
/// stubs below ([`clear_run_scratch`] per-run, [`free_session_heap`] at machine
/// drop) and moves `active_buffer` + persistent-root ownership onto the machine.
/// Until 1.A lands, this stays the wired teardown for the one-shot path.
pub fn clear_gc_state() {
    GC_STATE.with(|cell| {
        cell.borrow_mut().take();
    });
    clear_rust_roots();
}

/// PER-RUN teardown (Wave 1.A, component E′).
///
/// The half of [`clear_gc_state`] that is safe to run on every
/// `RegistryGuard::drop`: takes `GC_STATE` (dropping the `Option<GcState>`
/// wrapper but NOT the `active_buffer` Vec inside it — that was already
/// reclaimed back onto the machine by `reclaim_session_heap` before this
/// runs) and clears the per-run `RUST_ROOTS`. Does NOT touch
/// `PERSISTENT_ROOTS` — those are session-scoped and survive until
/// [`free_session_heap`] at machine drop.
pub fn clear_run_scratch() {
    GC_STATE.with(|c| {
        c.borrow_mut().take();
    });
    clear_rust_roots();
}

/// MACHINE-DROP teardown (Wave 1.A, component E′).
///
/// Clears the session-scoped `PERSISTENT_ROOTS` registry (whose slots point
/// into the machine-owned session heap) and takes `GC_STATE`. The session
/// heap `Vec<u8>` is a field on `JitEffectMachine` and drops with the
/// struct after this returns — this only clears the thread-local that held
/// pointers into that buffer so the GC cannot dereference them afterwards.
pub fn free_session_heap() {
    clear_persistent_roots();
    GC_STATE.with(|c| {
        c.borrow_mut().take();
    });
}

/// Install a retained session heap buffer as the active GC region.
///
/// Called by `install_registries` when a session machine has a previously-
/// retained `active_buffer` (the live heap after the first GC). The Vec is
/// moved into `GcState::active_buffer` so `perform_gc` can continue to
/// swap it in place. `reclaim_session_heap` later moves it back onto the
/// machine for the next run.
pub fn install_session_buffer(mut buffer: Vec<u8>) {
    let start = buffer.as_mut_ptr();
    let size = buffer.len();
    GC_STATE.with(|cell| {
        *cell.borrow_mut() = Some(GcState {
            active_start: start,
            active_size: size,
            active_buffer: Some(buffer),
        });
    });
}

/// Reclaim the live heap buffer (and the current high-water cursor) from
/// `GC_STATE` back onto the machine, called from `RegistryGuard::drop`
/// BEFORE `clear_run_scratch` takes the `GcState`.
///
/// Returns `(buffer, cursor)` where:
/// - `buffer` is `Some(Vec<u8>)` if a GC fired this run (the active_buffer
///   is the surviving to-space) or if a session buffer was installed at
///   re-entry; `None` only on a session's very first run with no GC (heap
///   still lives in the machine's `Nursery`).
/// - `cursor` is the number of bytes live at run end (the high-water mark
///   relative to the start of the active region), to resume allocation in
///   the next run.
///
/// # Safety
/// `alloc_ptr` must be the `VMContext::alloc_ptr` value at the end of the
/// run — the bump cursor after the last allocation.
pub fn reclaim_session_heap(alloc_ptr: *mut u8) -> (Option<Vec<u8>>, usize) {
    GC_STATE.with(|cell| match cell.borrow_mut().as_mut() {
        Some(state) => {
            let cursor = (alloc_ptr as usize).saturating_sub(state.active_start as usize);
            let buf = state.active_buffer.take();
            (buf, cursor)
        }
        None => (None, 0),
    })
}

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
    if check_cancel_and_set_error() {
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

/// Kill-switched fail-loud mode: `TIDEPOOL_HEAP_VERIFY=1` walks the entire
/// live set after every GC and panics on the first invariant violation.
/// Tests opt in; production pays one cached env read.
fn heap_verify_enabled() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("TIDEPOOL_HEAP_VERIFY").is_ok_and(|v| v == "1"))
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
        // Size is a u16 at byte offset 1 — intentionally unaligned in the
        // header layout; must be read_unaligned (debug builds abort on
        // misaligned derefs).
        let size = std::ptr::read_unaligned(obj.add(1) as *const u16) as usize;
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
    STACK_MAP_REGISTRY.with(|reg_cell| {
        if let Some(registry_ptr) = *reg_cell.borrow() {
            // SAFETY: registry_ptr was set by set_stack_map_registry and outlives JIT execution.
            let registry = unsafe { &*registry_ptr };
            // SAFETY: fp is a valid frame pointer read from gc_trigger's caller.
            // registry contains stack maps for all JIT functions in the call chain.
            let roots = unsafe { frame_walker::walk_frames(fp, registry) };

            // ── Cheney copying GC ──────────────────────────────
            GC_STATE.with(|gc_cell| {
                let mut gc_state = gc_cell.borrow_mut();
                if let Some(state) = gc_state.as_mut() {
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
                    RUST_ROOTS.with(|r| {
                        root_slots.extend(r.borrow().iter().copied());
                    });

                    // Append session-scoped persistent roots (Wave 1.A, component D).
                    // These survive across runs and are cleared only at machine drop.
                    PERSISTENT_ROOTS.with(|r| {
                        root_slots.extend(r.borrow().iter().copied());
                    });

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
            });
            // ── End GC ─────────────────────────────────────────
            let _ = roots; // roots consumed by cheney_copy; explicit drop for clarity
        }
    });
}

/// Set the stack map registry for the current thread.
///
/// # Safety
/// The registry must outlive any JIT code execution that might trigger GC, and should
/// be cleared (via `clear_stack_map_registry`) before the registry is dropped.
pub fn set_stack_map_registry(registry: &StackMapRegistry) {
    STACK_MAP_REGISTRY.with(|reg_cell| {
        *reg_cell.borrow_mut() = Some(registry as *const _);
    });
}

/// Clear the stack map registry for the current thread.
pub fn clear_stack_map_registry() {
    STACK_MAP_REGISTRY.with(|reg_cell| {
        *reg_cell.borrow_mut() = None;
    });
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
            std::ptr::write_unaligned(base.add(1) as *mut u16, 24);
            *base.add(layout::LIT_TAG_OFFSET as usize) = 0;
            *(base.add(layout::LIT_VALUE_OFFSET as usize) as *mut i64) = 42;
            // Con at offset 24: tag=2, size=32, con_tag, num_fields=1, field -> Lit.
            let con = base.add(24);
            *con = layout::TAG_CON;
            std::ptr::write_unaligned(con.add(1) as *mut u16, 32);
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
