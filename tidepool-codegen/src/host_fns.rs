//! Host functions for the JIT runtime — the Rust side of the JIT↔host ABI.
//!
//! Cranelift-emitted code calls into these symbols, which are handed to the JIT
//! module via `host_fn_symbols()` (the registration table near the bottom of the
//! file). Four concerns live here:
//!
//! - **Primop dispatch** — runtime implementations for primops not inlined as
//!   Cranelift IR (the bignum / `__gmpn_*` / `integer_gmp_*` intercepts, etc.).
//! - **GC trigger** — allocation slow-path callbacks into the copying collector,
//!   driven through the `VMContext` + stack-map registry.
//! - **Error poisoning** — `RuntimeError` raising: case traps, bad pointers,
//!   division by zero, and forced `error`/`undefined` sentinels.
//! - **Lazy-result streaming** — the `ValueSource` / `ValueStream` machinery that
//!   parks an effect-result iterator and serves it element-at-a-time.
//!
//! Always-on stderr breadcrumbs (`[CASE TRAP]`, `[BUG]`) fire only on genuine
//! compiler bugs and must stay loud.

use crate::context::VMContext;
use crate::gc::frame_walker;
use crate::layout;
use crate::stack_map::StackMapRegistry;
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use tidepool_heap::layout as heap_layout;

/// Addresses below this are considered invalid (null page guard).
const MIN_VALID_ADDR: u64 = 0x1000;

/// Runtime errors raised by JIT code via host functions.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RuntimeError {
    #[error("division by zero")]
    DivisionByZero,
    #[error("arithmetic overflow")]
    Overflow,
    #[error("Haskell error called")]
    UserError,
    #[error("Haskell undefined forced")]
    Undefined,
    #[error("case trap: scrutinee constructor not among case alternatives (tag mismatch; diagnostics on server stderr)")]
    CaseTrap,
    #[error("bad pointer in JIT runtime (diagnostics on server stderr)")]
    BadPointer,
    #[error("forced type metadata (should be dead code)")]
    TypeMetadata,
    #[error("unresolved variable VarId({0:#x}) [tag='{tag}', key={key}]", tag=(*.0 >> 56) as u8 as char, key=(*.0 & ((1u64 << 56) - 1)))]
    UnresolvedVar(u64),
    #[error("application of null function pointer")]
    NullFunPtr,
    #[error("application of non-closure (tag={0})")]
    BadFunPtrTag(u8),
    #[error("heap overflow (nursery exhausted after GC)")]
    HeapOverflow,
    #[error("stack overflow (likely infinite list or unbounded recursion — use zipWithIndex/imap/enumFromTo instead of [0..])")]
    StackOverflow,
    #[error("blackhole detected (infinite loop: thunk forced itself)")]
    BlackHole,
    #[error("thunk has invalid evaluation state: {0}")]
    BadThunkState(u8),
    #[error("Haskell error: {0}")]
    UserErrorMsg(String),
    /// External cancellation requested via a `CancelHandle`.
    /// Observed at the next GC safepoint (heap check).
    #[error("execution cancelled by external request")]
    Cancelled,
}

thread_local! {
    /// Registry of stack maps for JIT functions.
    /// This is set before calling into JIT code so gc_trigger can access it.
    static STACK_MAP_REGISTRY: RefCell<Option<*const StackMapRegistry>> = const { RefCell::new(None) };

    /// Runtime error from JIT code. Checked after JIT returns.
    static RUNTIME_ERROR: RefCell<Option<RuntimeError>> = const { RefCell::new(None) };

    pub(crate) static GC_STATE: RefCell<Option<GcState>> = const { RefCell::new(None) };

    /// Call depth counter for detecting runaway recursion (e.g. infinite lists).
    /// Reset before each JIT invocation; incremented in debug_app_check.
    static CALL_DEPTH: Cell<u32> = const { Cell::new(0) };

    /// Captured JIT diagnostics.
    static DIAGNOSTICS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };

    static EXEC_CONTEXT: RefCell<String> = const { RefCell::new(String::new()) };
    pub(crate) static SIGNAL_SAFE_CTX: Cell<[u8; 128]> = const { Cell::new([0u8; 128]) };
    pub(crate) static SIGNAL_SAFE_CTX_LEN: Cell<usize> = const { Cell::new(0) };

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

    /// External cancellation flag. When set, the next GC safepoint will abort the
    /// running program with `RuntimeError::Cancelled`. Cloned from the
    /// `Arc<AtomicBool>` owned by the `JitEffectMachine` before entering JIT code.
    ///
    /// Installed by `set_cancel_flag` (called from `JitEffectMachine::install_registries`)
    /// and cleared by `clear_cancel_flag` (called from `RegistryGuard::drop`).
    static CANCEL_FLAG: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
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

/// Set the current execution context for JIT code.
/// This is used to provide more info when a signal (SIGSEGV/SIGILL) occurs.
pub fn set_exec_context(ctx: &str) {
    EXEC_CONTEXT.with(|c| {
        let mut s = c.borrow_mut();
        s.clear();
        s.push_str(ctx);
    });
    SIGNAL_SAFE_CTX.with(|c| {
        let mut buf = [0u8; 128];
        let len = ctx.len().min(128);
        buf[..len].copy_from_slice(&ctx.as_bytes()[..len]);
        c.set(buf);
    });
    SIGNAL_SAFE_CTX_LEN.with(|c| c.set(ctx.len().min(128)));
}

/// Get the current execution context.
pub fn get_exec_context() -> String {
    EXEC_CONTEXT.with(|c| c.borrow().clone())
}

/// Push a diagnostic message to the thread-local buffer.
pub fn push_diagnostic(msg: String) {
    DIAGNOSTICS.with(|d| d.borrow_mut().push(msg));
}

/// Drain all accumulated diagnostics.
pub fn drain_diagnostics() -> Vec<String> {
    DIAGNOSTICS.with(|d| d.borrow_mut().drain(..).collect())
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

/// Install an external cancellation flag for the current thread. The next
/// GC safepoint (heap check) will observe the flag and abort the program with
/// `RuntimeError::Cancelled` if it has been set to `true`.
///
/// Called from `JitEffectMachine::install_registries` before entering JIT code.
pub(crate) fn set_cancel_flag(flag: Arc<AtomicBool>) {
    CANCEL_FLAG.with(|cell| {
        *cell.borrow_mut() = Some(flag);
    });
}

/// Remove the installed cancellation flag for the current thread. Called from
/// `RegistryGuard::drop` so the Arc is released even on an early error return.
pub(crate) fn clear_cancel_flag() {
    CANCEL_FLAG.with(|cell| {
        cell.borrow_mut().take();
    });
}

/// Fast check for an external cancel request. Uses a relaxed load — the cost
/// of a single extra relaxed atomic load per heap check is negligible, and
/// cancellation is best-effort (observed at the next safepoint) so stronger
/// ordering is not required.
#[inline]
fn cancel_requested() -> bool {
    CANCEL_FLAG.with(|cell| {
        cell.borrow()
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
    })
}

/// If cancellation has been requested, record `RuntimeError::Cancelled`
/// (unless another error is already pending) and return `true`. Callers
/// should then unwind by returning a poison pointer from their loop so the
/// outer run loop can surface the error.
#[inline]
pub(crate) fn check_cancel_and_set_error() -> bool {
    if cancel_requested() {
        RUNTIME_ERROR.with(|cell| {
            let mut slot = cell.borrow_mut();
            if slot.is_none() {
                *slot = Some(RuntimeError::Cancelled);
            }
        });
        true
    } else {
        false
    }
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
    // `if slot.is_none()` guard preserves the `Cancelled` cause so the
    // unwind surfaces correctly via `JitEffectMachine::run_pure`'s
    // `take_runtime_error()` check, not as `HeapOverflow`.
    //
    // Post-OOM stores into the poison are bounded by `POISON_BUF_SIZE`
    // (16 KiB, sized for worst-case Con writes — see PR #272).
    //
    // The other two cancel safepoints — the trampoline loop
    // (`check_cancel_and_set_error` in trampoline_resolve) and the
    // effect-dispatch boundary in `JitEffectMachine::run` — already
    // give prompt unwind for tail-recursive and effect-driven programs;
    // this path closes the gap for pure non-tail-call allocator loops
    // that never reach either (#273).
    if cancel_requested() {
        RUNTIME_ERROR.with(|cell| {
            let mut slot = cell.borrow_mut();
            if slot.is_none() {
                *slot = Some(RuntimeError::Cancelled);
            }
        });
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
                if lt as i64 > l::LIT_TAG_ARRAY {
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

/// Force a thunk to WHNF. Loops to handle chains (thunk returning thunk).
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn heap_force(vmctx: *mut VMContext, obj: *mut u8) -> *mut u8 {
    if obj.is_null() {
        return obj;
    }

    // SAFETY: obj is a valid heap pointer from the JIT nursery. The loop follows
    // indirection chains (thunks) and calls thunk entry functions via transmuted
    // code pointers stored in the thunk object. vmctx is passed through from JIT.
    unsafe {
        let mut current = obj;

        loop {
            let tag = heap_layout::read_tag(current);

            if tag == layout::TAG_THUNK {
                let state = *current.add(layout::THUNK_STATE_OFFSET as usize);
                match state {
                    layout::THUNK_UNEVALUATED => {
                        // 1. Mark blackhole for cycle detection
                        *current.add(layout::THUNK_STATE_OFFSET as usize) = layout::THUNK_BLACKHOLE;

                        // 2. Read code pointer
                        let code_ptr =
                            *(current.add(layout::THUNK_CODE_PTR_OFFSET as usize) as *const usize);

                        if code_ptr == 0 {
                            RUNTIME_ERROR.with(|cell| {
                                *cell.borrow_mut() = Some(RuntimeError::NullFunPtr);
                            });
                            return error_poison_ptr();
                        }

                        // 3. Call thunk entry function
                        // Signature: fn(vmctx, thunk_ptr) -> whnf_ptr
                        //
                        // The thunk code can allocate and trigger GC. `current` lives
                        // in this host frame, which the JIT frame walker deliberately
                        // skips, so it must be registered as an explicit Rust root:
                        // the copying GC frees from-space at the end of every
                        // collection, so a stale `current` would dangle into freed
                        // memory (post-call forwarding checks are unsound).
                        let f: extern "C" fn(*mut VMContext, *mut u8) -> *mut u8 =
                            std::mem::transmute(code_ptr);
                        let mark = rust_roots_mark();
                        register_rust_root(&mut current as *mut *mut u8);
                        let result = f(vmctx, current);
                        truncate_rust_roots(mark);

                        // If the thunk body raised an error (e.g. HeapOverflow
                        // from runtime_oom), memoize the poison result so
                        // re-forces follow the indirection instead of
                        // re-entering the failed body (which would GC-thrash
                        // until SIGSEGV). Then return poison immediately —
                        // don't loop into further forces.
                        if has_runtime_error() {
                            *(current.add(layout::THUNK_INDIRECTION_OFFSET as usize)
                                as *mut *mut u8) = result;
                            *current.add(layout::THUNK_STATE_OFFSET as usize) =
                                layout::THUNK_EVALUATED;
                            return error_poison_ptr();
                        }

                        debug_assert_ne!(
                            heap_layout::read_tag(current),
                            layout::TAG_FORWARDED,
                            "heap_force: registered root left forwarded"
                        );

                        // 4. Write indirection (offset 16, overwriting code_ptr)
                        *(current.add(layout::THUNK_INDIRECTION_OFFSET as usize) as *mut *mut u8) =
                            result;

                        // 5. Set state = Evaluated
                        *current.add(layout::THUNK_STATE_OFFSET as usize) = layout::THUNK_EVALUATED;

                        // Result may be another thunk — loop to force it
                        current = result;
                        continue;
                    }
                    layout::THUNK_BLACKHOLE => {
                        return runtime_blackhole_trap(vmctx);
                    }
                    layout::THUNK_EVALUATED => {
                        let next = *(current.add(layout::THUNK_INDIRECTION_OFFSET as usize)
                            as *const *mut u8);
                        current = next;
                        continue;
                    }
                    other => return runtime_bad_thunk_state_trap(vmctx, other),
                }
            }

            // Non-thunk tags (Closure, Con, Lit, unknown) — already WHNF.
            // Note: the pre-thunk closure-forcing path was removed because
            // TAG_THUNK now handles all lazy computations. TAG_CLOSURE objects
            // are genuine lambdas (with captures/args) and must not be called
            // with null arguments.
            return current;
        }
    }
}

/// Pointer stride of a `Con` field slot (one machine word). Matches the
/// `8 * index` field arithmetic in `effect_machine.rs` / `layout` Con reads.
const CON_FIELD_PTR_STRIDE: usize = 8;

/// Force a heap value to **normal form** (NF), iteratively (Wave 1.B, component K).
///
/// Unlike [`heap_force`] (WHNF — stops at the outermost constructor), this drives
/// the *entire* first-order (Tier-0) data spine to NF: it forces each node to
/// WHNF, then descends into every `Con` field and forces those too, writing the
/// forced pointer back into the field so the resulting graph holds no
/// unevaluated thunks. Closures / PAPs (`TAG_CLOSURE`) are **Tier-1**: forced to
/// WHNF but NOT descended into — a closure is a legitimate stored value, and
/// deep-forcing its captured environment has no NF meaning (and could diverge).
/// `Lit` leaves are already NF.
///
/// Iterative with an explicit work stack (no host recursion) — mirrors
/// `tidepool-eval`'s `deep_force` and the GC's `cheney_copy`, so an arbitrarily
/// deep structure (long list, deep tree) cannot overflow the host stack.
///
/// GC-safety: forcing a thunk runs JIT code that can allocate and trigger a
/// collection, relocating live objects. Every still-pending work item (a parent
/// heap pointer) plus the NF root is registered as a Rust GC root across each
/// [`heap_force`] call, so the copying GC rewrites them in place and no pending
/// pointer dangles. A field slot is recomputed from its (possibly relocated)
/// parent *after* the force, never cached across it.
///
/// Returns the (possibly relocated) NF root pointer, or the error poison pointer
/// if forcing raised a runtime error.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn deep_force(vmctx: *mut VMContext, root: *mut u8) -> *mut u8 {
    if root.is_null() {
        return root;
    }
    // SAFETY: `root` is a valid heap pointer; heap_force + the layout reads below
    // operate on valid heap objects; the rooting protocol (see doc comment) keeps
    // every pending pointer live and GC-updated across each collection.
    unsafe {
        // Force the root to WHNF first. heap_force roots its own argument across
        // the call, so a GC here is safe with no pending work items yet.
        let mut nf_root = heap_force(vmctx, root);
        if has_runtime_error() {
            return error_poison_ptr();
        }

        // Keep the NF root registered for the WHOLE descent: once a child is
        // popped off the work stack it is reachable only through the root graph,
        // so the root must stay live (and GC-updated) until we return it.
        let base_mark = rust_roots_mark();
        register_rust_root(&mut nf_root as *mut *mut u8);

        // Work items are (parent heap pointer, field index). Parents are exterior
        // heap-object pointers — GC-relocatable, and rewritten in place because we
        // register them; the field index is stable, so the field slot is
        // recomputed from the live parent after each force (never cached across a
        // collection).
        let mut work: Vec<(*mut u8, usize)> = Vec::new();
        push_con_fields(nf_root, &mut work);

        while let Some((mut parent, idx)) = work.pop() {
            // Register every pending parent + the current parent so a GC inside
            // the upcoming heap_force rewrites them all in place. (nf_root is
            // already registered at base_mark and stays so.)
            let mark = rust_roots_mark();
            for item in work.iter_mut() {
                register_rust_root(&mut item.0 as *mut *mut u8);
            }
            register_rust_root(&mut parent as *mut *mut u8);

            // Read the child from the live parent, force it, then write the NF
            // child back into the (possibly relocated) parent's field slot.
            let field_off = layout::CON_FIELDS_OFFSET as usize + idx * CON_FIELD_PTR_STRIDE;
            let child = *(parent.add(field_off) as *const *mut u8);
            let forced_child = heap_force(vmctx, child);
            truncate_rust_roots(mark);
            if has_runtime_error() {
                truncate_rust_roots(base_mark);
                return error_poison_ptr();
            }
            // `parent` may have moved during the force; recompute the slot.
            *(parent.add(field_off) as *mut *mut u8) = forced_child;

            // Descend into Tier-0 data only; Lits are leaves, Closures are Tier-1.
            push_con_fields(forced_child, &mut work);
        }

        truncate_rust_roots(base_mark);
        nf_root
    }
}

/// Push `(obj, i)` for each field index of a `Con` object onto `work`.
/// No-op for non-`Con` objects (`Lit` leaves; `Closure`/PAP = Tier-1, not
/// descended).
///
/// # Safety
/// `obj` must be a valid heap-object pointer.
unsafe fn push_con_fields(obj: *mut u8, work: &mut Vec<(*mut u8, usize)>) {
    if heap_layout::read_tag(obj) != layout::TAG_CON {
        return;
    }
    let n = *(obj.add(layout::CON_NUM_FIELDS_OFFSET as usize) as *const u16) as usize;
    for i in 0..n {
        work.push((obj, i));
    }
}

/// Resolve pending tail calls from VMContext. Called by non-tail App sites
/// when the callee returned null (indicating a tail call was stored).
///
/// Loop: read tail_callee+tail_arg from VMContext, clear them, call the closure,
/// check if result is null (another tail call) or a real value.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn trampoline_resolve(vmctx: *mut VMContext) -> *mut u8 {
    // SAFETY: vmctx is a valid pointer from JIT code. tail_callee/tail_arg are valid
    // heap pointers set by JIT tail-call sites. Code pointers in closures were set
    // during compilation and point to finalized JIT functions.
    unsafe {
        loop {
            // External cancellation safepoint. Tail-recursive loops never
            // return to the top-level JIT call on their own, so we must check
            // here — otherwise a runaway loop observes the cancel in
            // `gc_trigger`, receives a poison pointer from `runtime_oom`, and
            // immediately re-enters the trampoline forever. Returning the
            // poison here unwinds up to `JitEffectMachine::run_pure`, which
            // then surfaces `RuntimeError::Cancelled`.
            if check_cancel_and_set_error() {
                (*vmctx).tail_callee = std::ptr::null_mut();
                (*vmctx).tail_arg = std::ptr::null_mut();
                return error_poison_ptr();
            }

            // Runtime-error safepoint. A tail-recursive loop that exhausts the
            // heap gets a poison pointer from `runtime_oom` (which sets
            // `HeapOverflow`) but, like cancel, never returns to the top-level
            // JIT call on its own — it would re-enter the trampoline forever,
            // GC-thrashing a full heap until it corrupts and SIGSEGVs. Bail the
            // moment any error is pending so the poison unwinds to the run loop,
            // which surfaces the clean `HeapOverflow` (or other) error.
            if has_runtime_error() {
                (*vmctx).tail_callee = std::ptr::null_mut();
                (*vmctx).tail_arg = std::ptr::null_mut();
                return error_poison_ptr();
            }

            let callee = (*vmctx).tail_callee;
            let arg = (*vmctx).tail_arg;

            // Clear tail fields immediately
            (*vmctx).tail_callee = std::ptr::null_mut();
            (*vmctx).tail_arg = std::ptr::null_mut();

            if callee.is_null() {
                // No pending tail call — shouldn't happen, propagate null
                return std::ptr::null_mut();
            }

            // Reset call depth so tail-recursive loops don't hit the limit
            reset_call_depth();

            // Read code pointer from closure
            let code_ptr = *(callee.add(layout::CLOSURE_CODE_PTR_OFFSET as usize) as *const usize);

            // Call the closure: fn(vmctx, self, arg) -> result
            let func: unsafe extern "C" fn(*mut VMContext, *mut u8, *mut u8) -> *mut u8 =
                std::mem::transmute(code_ptr);
            let result = func(vmctx, callee, arg);

            if !result.is_null() {
                // Real return value — done
                return result;
            }

            // Result is null — check if another tail call was stored
            if (*vmctx).tail_callee.is_null() {
                // Null result with no pending tail call — propagate null (error)
                return std::ptr::null_mut();
            }

            // Another tail call pending — loop
        }
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

/// Called by JIT code when an unresolved external variable is forced.
/// Returns null to allow execution to continue (will likely segfault later).
/// In debug mode (TIDEPOOL_TRACE), logs and returns null.
pub extern "C" fn unresolved_var_trap(var_id: u64) -> *mut u8 {
    let tag_char = (var_id >> 56) as u8 as char;
    let key = var_id & ((1u64 << 56) - 1);
    let msg = format!(
        "[JIT] Forced unresolved external variable: VarId({:#x}) [tag='{}', key={}]",
        var_id, tag_char, key
    );
    eprintln!("{}", msg);
    push_diagnostic(msg);
    RUNTIME_ERROR.with(|cell| {
        *cell.borrow_mut() = Some(RuntimeError::UnresolvedVar(var_id));
    });
    error_poison_ptr()
}

/// Called by JIT code for runtime errors (divZeroError, overflowError).
/// Sets a thread-local error flag and returns a "poison" Lit(Int#, 0) object
/// instead of null. This prevents JIT code from segfaulting on the return value.
/// The effect machine checks the error flag after JIT returns and converts
/// to Yield::Error.
/// kind: 0 = divZeroError, 1 = overflowError, 2 = UserError, 3 = Undefined
pub extern "C" fn runtime_error(kind: u64) -> *mut u8 {
    let err_name = match kind {
        0 => "DivisionByZero",
        1 => "Overflow",
        2 => "UserError",
        3 => "Undefined",
        4 => "TypeMetadata",
        _ => "Unknown",
    };
    let msg = format!("[JIT] runtime_error called: kind={} ({})", kind, err_name);
    eprintln!("{}", msg);
    push_diagnostic(msg);
    let err = match kind {
        0 => RuntimeError::DivisionByZero,
        1 => RuntimeError::Overflow,
        2 => RuntimeError::UserError,
        3 => RuntimeError::Undefined,
        4 => RuntimeError::TypeMetadata,
        _ => RuntimeError::UserError,
    };
    RUNTIME_ERROR.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            *slot = Some(err);
        }
    });
    // Return a poison object instead of null. This is a valid Lit(Int#, 0)
    // heap object, so JIT code won't segfault when reading its tag byte.
    // The effect machine will detect the error flag and return Yield::Error
    // before this poison value reaches user code.
    error_poison_ptr()
}

/// Called by JIT code for runtime errors with a specific message.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn runtime_error_with_msg(kind: u64, msg_ptr: *const u8, msg_len: u64) -> *mut u8 {
    let msg = if !msg_ptr.is_null() && msg_len > 0 {
        // SAFETY: msg_ptr is non-null and points to msg_len bytes of valid memory
        // from a JIT-allocated LitString or leaked message buffer.
        let slice = unsafe { std::slice::from_raw_parts(msg_ptr, msg_len as usize) };
        String::from_utf8_lossy(slice).to_string()
    } else {
        String::new()
    };
    let err_name = match kind {
        0 => "DivisionByZero",
        1 => "Overflow",
        2 => "UserError",
        3 => "Undefined",
        4 => "TypeMetadata",
        _ => "Unknown",
    };
    let diag = format!(
        "[JIT] runtime_error called: kind={} ({}) msg={:?}",
        kind, err_name, msg
    );
    eprintln!("{}", diag);
    push_diagnostic(diag);
    let err = match kind {
        2 if !msg.is_empty() => RuntimeError::UserErrorMsg(msg),
        0 => RuntimeError::DivisionByZero,
        1 => RuntimeError::Overflow,
        2 => RuntimeError::UserError,
        3 => RuntimeError::Undefined,
        4 => RuntimeError::TypeMetadata,
        _ => RuntimeError::UserError,
    };
    RUNTIME_ERROR.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            *slot = Some(err);
        }
    });
    error_poison_ptr()
}

/// Returns true if a runtime error has been set for the current thread.
pub fn has_runtime_error() -> bool {
    RUNTIME_ERROR.with(|cell| cell.borrow().is_some())
}

pub extern "C" fn runtime_oom() -> *mut u8 {
    // Preserve a pre-existing runtime error if one is already set. The
    // external-cancellation path (see `gc_trigger`) sets `RuntimeError::Cancelled`
    // and then forces `runtime_oom` to fire; without this guard, `HeapOverflow`
    // would overwrite the more specific cancellation cause.
    RUNTIME_ERROR.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            *slot = Some(RuntimeError::HeapOverflow);
        }
    });
    error_poison_ptr()
}

/// Called by JIT code when a BlackHole is encountered (thunk forcing itself).
pub extern "C" fn runtime_blackhole_trap(_vmctx: *mut VMContext) -> *mut u8 {
    let msg = "[JIT] BlackHole detected: infinite loop (thunk forcing itself)".to_string();
    eprintln!("{}", msg);
    push_diagnostic(msg);
    RUNTIME_ERROR.with(|cell| {
        *cell.borrow_mut() = Some(RuntimeError::BlackHole);
    });
    error_poison_ptr()
}

/// Called by JIT code when a Thunk has an invalid state.
pub extern "C" fn runtime_bad_thunk_state_trap(_vmctx: *mut VMContext, state: u8) -> *mut u8 {
    let msg = format!("[JIT] Invalid thunk state: {}", state);
    eprintln!("{}", msg);
    push_diagnostic(msg);
    RUNTIME_ERROR.with(|cell| {
        *cell.borrow_mut() = Some(RuntimeError::BadThunkState(state));
    });
    error_poison_ptr()
}

/// Size of the poison buffer.
///
/// The JIT's `emit_alloc_fast_path` slow-fail edge calls `runtime_oom`, takes
/// the returned pointer as if it were a freshly-allocated heap object, and
/// then unconditionally writes the full header + payload into it (tag byte,
/// size halfword, Con/Closure/Thunk fields, capture slots, …). If the poison
/// is smaller than the attempted allocation, those post-OOM stores spill past
/// the poison into adjacent heap — we've observed glibc "corrupted size vs.
/// prev_size" aborts as a direct consequence.
///
/// The JIT never clamps allocation size at emit time. The effective upper
/// bound is `CON_FIELDS_OFFSET + MAX_FIELDS * 8` (i.e. the largest Con the
/// read-side `heap_bridge` is willing to decode; see `MAX_FIELDS = 1024`
/// there). Closures and thunks are bounded by the same field/capture count
/// in practice. We size the poison to comfortably absorb that worst case so
/// any OOM path can complete its field writes harmlessly.
///
/// 16 KiB: `24 + 8 * 1024 = 8216` bytes for a max-arity Con, doubled for
/// headroom. Stays well under the `u16` header `size` encoding limit.
pub(crate) const POISON_BUF_SIZE: usize = 16 * 1024;

/// Compile-time guard: the poison buffer must be large enough to absorb a
/// post-OOM write of a worst-case Con at the read-side decoder's
/// `MAX_FIELDS` ceiling. If `MAX_FIELDS` is bumped without updating
/// `POISON_BUF_SIZE`, this assertion fails to compile rather than
/// regressing into the runtime heap-corruption symptom that PR #272
/// originally diagnosed (glibc "corrupted size vs. prev_size" aborts on
/// OOM paths writing past the old 24-byte poison). The matching runtime
/// regression test lives in the module's `tests` block under
/// `poison_buf_absorbs_max_con_write`.
const _: () = {
    let worst_case_con = layout::CON_FIELDS_OFFSET as usize + crate::heap_bridge::MAX_FIELDS * 8;
    assert!(
        POISON_BUF_SIZE >= worst_case_con,
        "POISON_BUF_SIZE must absorb worst-case Con write \
         (CON_FIELDS_OFFSET + MAX_FIELDS * 8); bump POISON_BUF_SIZE \
         when MAX_FIELDS grows",
    );
};

/// Return a pointer to a pre-allocated "poison" Closure heap object.
/// When JIT code tries to call this as a function, it returns itself,
/// preventing cascading crashes. The runtime error flag is already set,
/// so the effect machine will catch it before the poison reaches user code.
///
/// The backing allocation is oversized (`POISON_BUF_SIZE`) so that OOM
/// paths which treat the poison as freshly-allocated scratch (via
/// `runtime_oom`) can complete their field writes without corrupting
/// adjacent heap. See `POISON_BUF_SIZE` for rationale.
pub fn error_poison_ptr() -> *mut u8 {
    use std::sync::OnceLock;
    // Layout: Closure with code_ptr pointing to `poison_trampoline`,
    // num_captured = 0. When called, returns the poison closure itself.
    static POISON: OnceLock<usize> = OnceLock::new();
    let addr = *POISON.get_or_init(|| {
        // Backing buffer is oversized to absorb post-OOM scratch writes
        // from the JIT (see POISON_BUF_SIZE docs). The Closure header
        // describes only the logical 24-byte Closure layout — the tail
        // bytes are zero-initialized padding that the JIT may clobber
        // after a `runtime_oom` return.
        let logical_size = 24u16;
        let layout = std::alloc::Layout::from_size_align(POISON_BUF_SIZE, 8)
            .unwrap_or_else(|_| std::process::abort());
        // SAFETY: alloc_zeroed returns a valid, zeroed allocation of the requested size.
        let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
        if ptr.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        // SAFETY: ptr is a fresh allocation of POISON_BUF_SIZE bytes
        // (>= 24). Writing the closure header, code pointer, and capture
        // count at known offsets within the first 24 bytes.
        unsafe {
            tidepool_heap::layout::write_header(
                ptr,
                tidepool_heap::layout::TAG_CLOSURE,
                logical_size,
            );
            // code_ptr = poison_trampoline
            *(ptr.add(tidepool_heap::layout::CLOSURE_CODE_PTR_OFFSET) as *mut usize) =
                poison_trampoline as *const () as usize;
            // num_captured = 0
            *(ptr.add(tidepool_heap::layout::CLOSURE_NUM_CAPTURED_OFFSET) as *mut u16) = 0;
        }
        ptr as usize
    });
    addr as *mut u8
}

/// Trampoline for the poison closure. Returns the poison closure itself,
/// so any chain of function applications on an error result just keeps
/// returning the poison without crashing.
// SAFETY: Only called via JIT code applying the poison closure. Returns the
// static poison pointer — no memory writes, no side effects beyond the return.
unsafe extern "C" fn poison_trampoline(
    _vmctx: *mut VMContext,
    _closure: *mut u8,
    _arg: *mut u8,
) -> *mut u8 {
    error_poison_ptr()
}

/// Return a pre-allocated "lazy poison" Closure for a given error kind.
/// Unlike `error_poison_ptr()`, this does NOT set the error flag at creation
/// time. The error is only triggered when the closure is actually called
/// (via `poison_trampoline_lazy`). This is critical for typeclass dictionaries
/// where error methods exist as fields but may never be invoked.
///
/// kind: 0=DivisionByZero, 1=Overflow, 2=UserError, 3=Undefined, 4=TypeMetadata
pub fn error_poison_ptr_lazy(kind: u64) -> *mut u8 {
    use std::sync::OnceLock;
    static LAZY_POISONS: OnceLock<[usize; 5]> = OnceLock::new();
    let ptrs = LAZY_POISONS.get_or_init(|| {
        let mut arr = [0usize; 5];
        for k in 0..5u64 {
            // Closure: header(8) + code_ptr(8) + num_captured(2+pad=8) + captured[0](8) = 32
            let size = 32usize;
            let lo = std::alloc::Layout::from_size_align(size, 8)
                .unwrap_or_else(|_| std::process::abort());
            // SAFETY: alloc_zeroed returns a valid, zeroed allocation of the requested size.
            let ptr = unsafe { std::alloc::alloc_zeroed(lo) };
            if ptr.is_null() {
                std::alloc::handle_alloc_error(lo);
            }
            // SAFETY: ptr is a fresh 32-byte allocation. Writing closure header, code pointer,
            // capture count, and captured error kind at known offsets.
            unsafe {
                tidepool_heap::layout::write_header(
                    ptr,
                    tidepool_heap::layout::TAG_CLOSURE,
                    size as u16,
                );
                *(ptr.add(tidepool_heap::layout::CLOSURE_CODE_PTR_OFFSET) as *mut usize) =
                    poison_trampoline_lazy as *const () as usize;
                *(ptr.add(tidepool_heap::layout::CLOSURE_NUM_CAPTURED_OFFSET) as *mut u16) = 1;
                *(ptr.add(tidepool_heap::layout::CLOSURE_CAPTURED_OFFSET) as *mut u64) = k;
            }
            arr[k as usize] = ptr as usize;
        }
        arr
    });
    ptrs[kind.min(4) as usize] as *mut u8
}

/// Raise a runtime error whose message is materialized from a live heap value.
/// Called by JIT Raise sites whose message wasn't statically extractable
/// (floated bindings, thunk-subtree captures, dynamically built messages).
/// Handles String literals, Text constructors, and String cons-lists; falls
/// back to a message-less error on any other shape.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn runtime_error_dynamic(vmctx: *mut VMContext, kind: u64, arg: *mut u8) -> *mut u8 {
    // SAFETY: vmctx and arg come from JIT code; arg may be null.
    let msg = unsafe { materialize_message(vmctx, arg) };
    match msg {
        Some(bytes) if !bytes.is_empty() => {
            // runtime_error_with_msg copies the bytes into an owned String.
            runtime_error_with_msg(kind, bytes.as_ptr(), bytes.len() as u64)
        }
        _ => runtime_error(kind),
    }
}

/// Best-effort conversion of a heap value into UTF-8 message bytes.
/// Forces thunks as needed; all locals held across forces are registered as
/// GC roots (forcing can collect, and host frames are invisible to the
/// frame walker).
///
/// # Safety
/// `vmctx` must be valid; `arg` must be null or a valid heap object.
unsafe fn materialize_message(vmctx: *mut VMContext, arg: *mut u8) -> Option<Vec<u8>> {
    const MAX_MSG_BYTES: usize = 4096;
    if vmctx.is_null() || arg.is_null() {
        return None;
    }

    let mark = rust_roots_mark();
    let mut cur: *mut u8 = arg;
    let mut tmp: *mut u8 = std::ptr::null_mut();
    register_rust_root(&mut cur as *mut *mut u8);
    register_rust_root(&mut tmp as *mut *mut u8);

    // Reads an Int/Char payload, looking through an I#/C# box.
    // Does not force; callers force into `tmp` first.
    let read_small_int = |p: *mut u8| -> Option<i64> {
        match heap_layout::read_tag(p) {
            t if t == tidepool_heap::layout::TAG_LIT => {
                Some(*(p.add(tidepool_heap::layout::LIT_VALUE_OFFSET) as *const i64))
            }
            t if t == tidepool_heap::layout::TAG_CON => {
                let nf = *(p.add(tidepool_heap::layout::CON_NUM_FIELDS_OFFSET) as *const u16);
                if nf == 1 {
                    let f0 = *(p.add(tidepool_heap::layout::CON_FIELDS_OFFSET) as *const *mut u8);
                    if !f0.is_null() && heap_layout::read_tag(f0) == tidepool_heap::layout::TAG_LIT
                    {
                        return Some(
                            *(f0.add(tidepool_heap::layout::LIT_VALUE_OFFSET) as *const i64),
                        );
                    }
                }
                None
            }
            _ => None,
        }
    };

    let read_lit_string = |p: *mut u8| -> Option<Vec<u8>> {
        if heap_layout::read_tag(p) != tidepool_heap::layout::TAG_LIT {
            return None;
        }
        // LitString and ByteArray# share the [len: u64][bytes...] payload
        // layout; Text's first field is a ByteArray#.
        let lit_tag = *p.add(tidepool_heap::layout::LIT_TAG_OFFSET);
        if lit_tag != 5 && lit_tag != crate::layout::LIT_TAG_BYTEARRAY as u8 {
            // 5 = LIT_TAG_STRING
            return None;
        }
        let raw = *(p.add(tidepool_heap::layout::LIT_VALUE_OFFSET) as *const *const u8);
        if raw.is_null() {
            return None;
        }
        let len = (*(raw as *const u64) as usize).min(MAX_MSG_BYTES);
        Some(std::slice::from_raw_parts(raw.add(8), len).to_vec())
    };

    let result = (|| -> Option<Vec<u8>> {
        if is_lazy_poison(cur) {
            return None;
        }
        cur = heap_force(vmctx, cur);
        if has_runtime_error() {
            return None;
        }

        // Bare string literal.
        if let Some(bytes) = read_lit_string(cur) {
            return Some(bytes);
        }

        if heap_layout::read_tag(cur) != tidepool_heap::layout::TAG_CON {
            return None;
        }
        let nf = *(cur.add(tidepool_heap::layout::CON_NUM_FIELDS_OFFSET) as *const u16);

        // Text bytes offset len — field 0 holds the byte buffer, possibly
        // behind single-field box constructors (ByteArray ba#).
        if nf == 3 {
            tmp = *(cur.add(tidepool_heap::layout::CON_FIELDS_OFFSET) as *const *mut u8);
            if tmp.is_null() {
                return None;
            }
            tmp = heap_force(vmctx, tmp);
            while heap_layout::read_tag(tmp) == tidepool_heap::layout::TAG_CON
                && *(tmp.add(tidepool_heap::layout::CON_NUM_FIELDS_OFFSET) as *const u16) == 1
            {
                let inner = *(tmp.add(tidepool_heap::layout::CON_FIELDS_OFFSET) as *const *mut u8);
                if inner.is_null() {
                    return None;
                }
                tmp = heap_force(vmctx, inner);
            }
            let bytes = read_lit_string(tmp)?;
            // Re-read offset/len AFTER the force above (cur may have moved).
            let f1 = *(cur.add(tidepool_heap::layout::CON_FIELDS_OFFSET + 8) as *const *mut u8);
            let f2 = *(cur.add(tidepool_heap::layout::CON_FIELDS_OFFSET + 16) as *const *mut u8);
            let off = read_small_int(f1)? as usize;
            let len = read_small_int(f2)? as usize;
            if off <= bytes.len() {
                let end = (off + len).min(bytes.len());
                return Some(bytes[off..end].to_vec());
            }
            return None;
        }

        // String cons-list of Chars.
        let mut out: Vec<u8> = Vec::new();
        loop {
            let tag = heap_layout::read_tag(cur);
            if tag != tidepool_heap::layout::TAG_CON {
                break;
            }
            let nf = *(cur.add(tidepool_heap::layout::CON_NUM_FIELDS_OFFSET) as *const u16);
            if nf != 2 {
                break; // nil (0 fields) or not a list shape
            }
            tmp = *(cur.add(tidepool_heap::layout::CON_FIELDS_OFFSET) as *const *mut u8);
            if tmp.is_null() {
                break;
            }
            tmp = heap_force(vmctx, tmp);
            if has_runtime_error() {
                break;
            }
            let Some(c) = read_small_int(tmp) else { break };
            let Some(ch) = char::from_u32(c as u32) else {
                break;
            };
            let mut buf = [0u8; 4];
            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            if out.len() >= MAX_MSG_BYTES {
                break;
            }
            // Re-read the tail AFTER forcing the head (cur may have moved).
            let next = *(cur.add(tidepool_heap::layout::CON_FIELDS_OFFSET + 8) as *const *mut u8);
            if next.is_null() {
                break;
            }
            cur = heap_force(vmctx, next);
            if has_runtime_error() {
                break;
            }
        }
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    })();

    truncate_rust_roots(mark);
    result
}

/// Check whether a heap object is a lazy poison closure (⊥ with a deferred
/// error). Used by the heap bridge: converting ⊥ to a `Value` is a genuine
/// demand, so the bridge invokes the trampoline to raise the deferred error
/// (with its captured message) instead of misreading the closure as data.
/// Deliberately NOT consulted by `heap_force`: dictionaries carry poison in
/// never-selected method slots, and effect plumbing forces bound values it
/// must not observe.
///
/// # Safety
/// `ptr` must be a valid heap object pointer.
pub unsafe fn is_lazy_poison(ptr: *const u8) -> bool {
    if heap_layout::read_tag(ptr as *mut u8) != tidepool_heap::layout::TAG_CLOSURE {
        return false;
    }
    let code_ptr = *(ptr.add(tidepool_heap::layout::CLOSURE_CODE_PTR_OFFSET) as *const usize);
    code_ptr == poison_trampoline_lazy as *const () as usize
        || code_ptr == poison_trampoline_lazy_msg as *const () as usize
}

/// Invoke a lazy poison closure's trampoline, setting the runtime error flag
/// (including any captured message) and returning the eager poison pointer.
///
/// # Safety
/// `ptr` must satisfy `is_lazy_poison`.
pub unsafe fn raise_lazy_poison(vmctx: *mut VMContext, ptr: *mut u8) -> *mut u8 {
    let code_ptr = *(ptr.add(tidepool_heap::layout::CLOSURE_CODE_PTR_OFFSET) as *const usize);
    let f: unsafe extern "C" fn(*mut VMContext, *mut u8, *mut u8) -> *mut u8 =
        std::mem::transmute(code_ptr);
    f(vmctx, ptr, std::ptr::null_mut())
}

/// Trampoline for lazy poison closures. Reads the error kind from captured[0]
/// and raises — setting the error flag only now, when the closure is actually
/// invoked. The argument is the error's message expression whenever the
/// sentinel was applied (including first-class uses like the point-free
/// `error . unpack` shadow, where the poison closure receives the already
/// computed String at call time): materialize it into the message.
// SAFETY: closure points to a lazy poison closure allocated by error_poison_ptr_lazy
// with captured[0] = error kind. arg may be null or a valid heap object.
unsafe extern "C" fn poison_trampoline_lazy(
    vmctx: *mut VMContext,
    closure: *mut u8,
    arg: *mut u8,
) -> *mut u8 {
    let kind = *(closure.add(tidepool_heap::layout::CLOSURE_CAPTURED_OFFSET) as *const u64);

    if let Some(bytes) = materialize_message(vmctx, arg) {
        if !bytes.is_empty() {
            return runtime_error_with_msg(kind, bytes.as_ptr(), bytes.len() as u64);
        }
    }

    // Non-string argument (e.g. the CallStack dict in a partial application
    // like `error cs`): swallow it and return self, so the eventual
    // application to the actual message raises with that message. A poison
    // that is forced as a value (never applied) reaches the bridge, which
    // raises message-less via raise_lazy_poison(null).
    if !arg.is_null() {
        return closure;
    }

    runtime_error(kind)
}

/// Create a pre-allocated "lazy poison" Closure for a given error kind and message.
pub fn error_poison_ptr_lazy_msg(kind: u64, msg: &[u8]) -> *mut u8 {
    // Leak the message bytes so they live forever
    let msg_bytes = msg.to_vec().into_boxed_slice();
    let msg_ptr = msg_bytes.as_ptr();
    let msg_len = msg_bytes.len();
    std::mem::forget(msg_bytes);

    // Allocate closure with 3 captures: kind, msg_ptr, msg_len
    // Closure: header(8) + code_ptr(8) + num_captured(2+pad=8) + 3*8 = 48
    let size = tidepool_heap::layout::CLOSURE_CAPTURED_OFFSET + 3 * 8;
    let layout = std::alloc::Layout::from_size_align(size, 8).expect("constant size/align");
    // SAFETY: alloc_zeroed returns a valid, zeroed allocation of the requested size.
    let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
    if ptr.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
    // SAFETY: ptr is a fresh allocation. Writing closure header, code pointer,
    // capture count, and 3 captures (kind, msg_ptr, msg_len) at known offsets.
    // msg_ptr is a leaked allocation that lives forever.
    unsafe {
        tidepool_heap::layout::write_header(ptr, tidepool_heap::layout::TAG_CLOSURE, size as u16);
        *(ptr.add(tidepool_heap::layout::CLOSURE_CODE_PTR_OFFSET) as *mut usize) =
            poison_trampoline_lazy_msg as *const () as usize;
        *(ptr.add(tidepool_heap::layout::CLOSURE_NUM_CAPTURED_OFFSET) as *mut u16) = 3;
        *(ptr.add(tidepool_heap::layout::CLOSURE_CAPTURED_OFFSET) as *mut u64) = kind;
        *(ptr.add(tidepool_heap::layout::CLOSURE_CAPTURED_OFFSET + 8) as *mut usize) =
            msg_ptr as usize;
        *(ptr.add(tidepool_heap::layout::CLOSURE_CAPTURED_OFFSET + 16) as *mut u64) =
            msg_len as u64;
    }
    ptr
}

// SAFETY: closure points to a lazy poison closure with 3 captures (kind, msg_ptr, msg_len)
// allocated by error_poison_ptr_lazy_msg. The msg_ptr was leaked and remains valid.
unsafe extern "C" fn poison_trampoline_lazy_msg(
    _vmctx: *mut VMContext,
    closure: *mut u8,
    _arg: *mut u8,
) -> *mut u8 {
    let kind = *(closure.add(tidepool_heap::layout::CLOSURE_CAPTURED_OFFSET) as *const u64);
    let msg_ptr =
        *(closure.add(tidepool_heap::layout::CLOSURE_CAPTURED_OFFSET + 8) as *const *const u8);
    let msg_len = *(closure.add(tidepool_heap::layout::CLOSURE_CAPTURED_OFFSET + 16) as *const u64);
    runtime_error_with_msg(kind, msg_ptr, msg_len)
}

/// Check and take any pending runtime error from JIT code.
///
/// Uses `try_borrow_mut` defensively: this runs on the signal/teardown path
/// (`runtime_error_or_signal` and `RegistryGuard::drop`), and a signal can fire
/// while JIT host code (e.g. `debug_app_check` setting `StackOverflow`) still
/// holds a `borrow_mut` on the cell. A plain `borrow_mut` would then panic —
/// and panicking inside `Drop`/unwind double-panics → `abort()`. If the cell is
/// momentarily borrowed, there is no error we can safely take here; return None
/// and let the caller fall back (e.g. `Signal(sig)`).
pub fn take_runtime_error() -> Option<RuntimeError> {
    RUNTIME_ERROR.with(|cell| cell.try_borrow_mut().ok().and_then(|mut e| e.take()))
}

/// Reset the call depth counter. Call before each JIT invocation.
pub fn reset_call_depth() {
    CALL_DEPTH.with(|c| c.set(0));
}

/// Check pointer validity; if bad, set runtime error and return true.
fn check_ptr_invalid(ptr: *const u8, fn_name: &str) -> bool {
    if (ptr as i64) < MIN_VALID_ADDR as i64 {
        let msg = format!("[BUG] {}: bad pointer {:#x}", fn_name, ptr as u64);
        eprintln!("{}", msg);
        push_diagnostic(msg);
        RUNTIME_ERROR.with(|cell| {
            *cell.borrow_mut() = Some(RuntimeError::BadPointer);
        });
        true
    } else {
        false
    }
}

/// Return the list of host function symbols for JIT registration.
///
/// Usage: `CodegenPipeline::new(&host_fn_symbols())`
/// Debug: called before every App call_indirect to validate the function pointer.
/// Prints the heap tag and code_ptr. Aborts on non-closure.
///
/// # Safety
///
/// `fun_ptr` must point to a valid HeapObject if not null.
/// Maximum call depth before raising StackOverflow. This catches infinite
/// recursion (e.g. `[0..]` in non-fusing context) with a clean error
/// instead of SIGSEGV from stack overflow.
const MAX_CALL_DEPTH: u32 = 20_000;

/// Returns 0 if the call is safe to proceed, or a poison pointer if the call
/// should be short-circuited (runtime error already set or call depth exceeded).
///
/// # Safety
/// fun_ptr must point to a valid HeapObject or be null.
pub unsafe extern "C" fn debug_app_check(fun_ptr: *const u8) -> *mut u8 {
    // If a runtime error is already pending, don't abort on tag mismatches —
    // we're in error-propagation mode and the effect machine will handle it.
    let has_error = RUNTIME_ERROR.with(|cell| cell.borrow().is_some());

    // Check call depth to catch runaway recursion before stack overflow.
    if !has_error {
        let depth = CALL_DEPTH.with(|c| {
            let d = c.get() + 1;
            c.set(d);
            d
        });
        if depth > MAX_CALL_DEPTH {
            RUNTIME_ERROR.with(|cell| {
                *cell.borrow_mut() = Some(RuntimeError::StackOverflow);
            });
            return error_poison_ptr();
        }
    }
    if fun_ptr.is_null() {
        if has_error {
            return error_poison_ptr(); // Error already flagged, just continue
        }
        let msg = "[JIT] App: fun_ptr is NULL — unresolved binding".to_string();
        eprintln!("{}", msg);
        push_diagnostic(msg);
        RUNTIME_ERROR.with(|cell| {
            *cell.borrow_mut() = Some(RuntimeError::NullFunPtr);
        });
        return error_poison_ptr();
    }
    // SAFETY: fun_ptr was checked non-null above; reading the tag byte at offset 0
    // of a heap object is valid for any object allocated by the JIT nursery.
    let tag = unsafe { *fun_ptr };
    if tag != tidepool_heap::layout::TAG_CLOSURE {
        use std::io::Write;
        let mut stderr = std::io::stderr().lock();
        if has_error {
            return error_poison_ptr(); // Error already flagged, tag mismatch is expected (poison object)
        }
        let tag_name = match tag {
            0 => "Closure",
            1 => "Thunk",
            2 => "Con",
            3 => "Lit",
            _ => "UNKNOWN",
        };
        let msg = format!(
            "[JIT] App: fun_ptr={:p} has tag {} ({}) — expected Closure!",
            fun_ptr, tag, tag_name
        );
        let _ = writeln!(stderr, "{}", msg);
        push_diagnostic(msg);
        if tag == tidepool_heap::layout::TAG_CON {
            // SAFETY: tag == TAG_CON confirms this is a Con heap object;
            // reading con_tag at offset 8 and num_fields at offset 16 is valid.
            let con_tag = unsafe { *(fun_ptr.add(layout::CON_TAG_OFFSET as usize) as *const u64) };
            let num_fields =
                unsafe { *(fun_ptr.add(layout::CON_NUM_FIELDS_OFFSET as usize) as *const u16) };
            let msg2 = format!("[JIT]   Con tag={}, num_fields={}", con_tag, num_fields);
            let _ = writeln!(stderr, "{}", msg2);
            push_diagnostic(msg2);
        }
        let _ = stderr.flush();
        RUNTIME_ERROR.with(|cell| {
            *cell.borrow_mut() = Some(RuntimeError::BadFunPtrTag(tag));
        });
        return error_poison_ptr();
    }
    std::ptr::null_mut() // 0 = ok, proceed with the call
}

// ---------------------------------------------------------------------------
// ByteArray runtime functions
// ---------------------------------------------------------------------------

/// Allocate a new mutable byte array of `size` bytes, zeroed.
/// Layout: [u64 length][u8 bytes...]
/// Returns a raw pointer to the allocation (caller stores in Lit value slot).
/// Mutable byte arrays are malloc'd with a hidden capacity word BELOW the
/// returned pointer:
///
/// ```text
///   base: [u64 total alloc size][u64 logical len][data ...]
///                                ^returned ba     ^ba + 8
/// ```
///
/// The JIT ABI (logical length at `ba`, data at `ba + 8`) is unchanged.
/// `runtime_shrink_byte_array` rewrites only the LOGICAL length, so the
/// capacity word is the only sound source for the dealloc `Layout` in
/// `runtime_resize_byte_array` — deriving it from the (possibly shrunk)
/// logical prefix deallocated with the wrong layout, which is UB.
/// (proptest_host_arrays BUG-2)
const BYTE_ARRAY_BASE_OFFSET: usize = 8;

pub extern "C" fn runtime_new_byte_array(size: i64) -> i64 {
    if size < 0 {
        RUNTIME_ERROR.with(|cell| {
            *cell.borrow_mut() = Some(RuntimeError::UserErrorMsg(
                "negative size in byte array allocation".to_string(),
            ));
        });
        return error_poison_ptr() as i64;
    }
    let total = (2 * BYTE_ARRAY_BASE_OFFSET).saturating_add(size as usize);
    let layout =
        std::alloc::Layout::from_size_align(total, 8).unwrap_or_else(|_| std::process::abort());
    // SAFETY: alloc_zeroed returns a valid, zeroed allocation of the requested size.
    let base = unsafe { std::alloc::alloc_zeroed(layout) };
    if base.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
    // SAFETY: base is a valid fresh allocation; capacity word at offset 0,
    // logical length prefix at offset 8 (= the returned ba's offset 0).
    unsafe {
        *(base as *mut u64) = total as u64;
        let ba = base.add(BYTE_ARRAY_BASE_OFFSET);
        *(ba as *mut u64) = size as u64;
        ba as i64
    }
}

/// Copy `len` bytes from `src` (Addr#) to `dest_ba` (ByteArray ptr) at `dest_off`.
pub extern "C" fn runtime_copy_addr_to_byte_array(src: i64, dest_ba: i64, dest_off: i64, len: i64) {
    if check_ptr_invalid(src as *const u8, "runtime_copy_addr_to_byte_array")
        || check_ptr_invalid(dest_ba as *const u8, "runtime_copy_addr_to_byte_array")
    {
        return;
    }
    if dest_off < 0 || len < 0 {
        return;
    }
    // SAFETY: dest_ba passed the null-guard above and points to a byte array
    // with a u64 length prefix at offset 0.
    let dest_size = unsafe { *(dest_ba as *const u64) } as usize;
    if (dest_off as usize).saturating_add(len as usize) > dest_size {
        return;
    }
    let src_ptr = src as *const u8;
    // SAFETY: dest_ba + 8 + dest_off is within the byte array (bounds checked above).
    let dest_ptr = unsafe { (dest_ba as *mut u8).add(8 + dest_off as usize) };
    // SAFETY: src is a valid Addr# from JIT code, dest is within bounds, and regions
    // do not overlap (src is external memory, dest is a byte array).
    unsafe {
        std::ptr::copy_nonoverlapping(src_ptr, dest_ptr, len as usize);
    }
}

/// Set `len` bytes in `ba` starting at `off` to `val`.
pub extern "C" fn runtime_set_byte_array(ba: i64, off: i64, len: i64, val: i64) {
    if check_ptr_invalid(ba as *const u8, "runtime_set_byte_array") {
        return;
    }
    if off < 0 || len < 0 {
        return;
    }
    let ba_size = unsafe { *(ba as *const u64) } as usize;
    if (off as usize).saturating_add(len as usize) > ba_size {
        return;
    }
    // SAFETY: ba passed the null-guard above; offsetting past the 8-byte length prefix + off.
    let ptr = unsafe { (ba as *mut u8).add(8 + off as usize) };
    // SAFETY: ptr is within the byte array allocation.
    unsafe {
        std::ptr::write_bytes(ptr, val as u8, len as usize);
    }
}

/// Shrink a mutable byte array to `new_size` bytes (just updates the length prefix).
pub extern "C" fn runtime_shrink_byte_array(ba: i64, new_size: i64) {
    if new_size < 0 || (ba as u64) < MIN_VALID_ADDR {
        return;
    }
    let old_size = unsafe { *(ba as *const u64) } as i64;
    if new_size > old_size {
        return; // only allow shrink, not grow
    }
    // SAFETY: ba is a valid byte array pointer from JIT code. Writing the length
    // prefix at offset 0 with a smaller value (logical shrink, no reallocation).
    unsafe {
        *(ba as *mut u64) = new_size as u64;
    }
}

/// Resize a mutable byte array. Allocates a new buffer, copies existing data,
/// zeroes any new bytes, and frees the old buffer. Returns the new pointer.
pub extern "C" fn runtime_resize_byte_array(ba: i64, new_size: i64) -> i64 {
    if new_size < 0 {
        RUNTIME_ERROR.with(|cell| {
            *cell.borrow_mut() = Some(RuntimeError::UserErrorMsg(
                "negative size in byte array allocation".to_string(),
            ));
        });
        return error_poison_ptr() as i64;
    }
    if (ba as u64) < MIN_VALID_ADDR {
        return error_poison_ptr() as i64;
    }
    let old_ptr = ba as *mut u8;
    // SAFETY: old_ptr passed the validity check above; logical length prefix at
    // offset 0, hidden capacity word at offset -8 (see runtime_new_byte_array).
    let old_size = unsafe { *(old_ptr as *const u64) } as usize;
    let old_base = unsafe { old_ptr.sub(BYTE_ARRAY_BASE_OFFSET) };
    // The TRUE allocation size — independent of any logical shrink since
    // allocation. Deriving the dealloc layout from the logical prefix after a
    // shrink(M) deallocated with size 8+M instead of the allocated size: UB.
    // (proptest_host_arrays BUG-2)
    let old_total = unsafe { *(old_base as *const u64) } as usize;
    let new_size = new_size as usize;

    let new_total = (2 * BYTE_ARRAY_BASE_OFFSET).saturating_add(new_size);
    let new_layout =
        std::alloc::Layout::from_size_align(new_total, 8).unwrap_or_else(|_| std::process::abort());
    // SAFETY: alloc_zeroed returns a valid, zeroed allocation of the requested size.
    let new_base = unsafe { std::alloc::alloc_zeroed(new_layout) };
    if new_base.is_null() {
        std::alloc::handle_alloc_error(new_layout);
    }
    let new_ptr = unsafe { new_base.add(BYTE_ARRAY_BASE_OFFSET) };

    // Copy existing data (up to min of old/new logical size)
    let copy_len = old_size.min(new_size);
    // SAFETY: Both old and new buffers have data starting at offset 8 past the
    // logical prefix. copy_len <= min(old_size, new_size) so reads/writes are in
    // bounds (logical size never exceeds backing capacity).
    unsafe {
        std::ptr::copy_nonoverlapping(old_ptr.add(8), new_ptr.add(8), copy_len);
    }

    // SAFETY: fresh allocation; capacity word at base, logical prefix at ba.
    unsafe {
        *(new_base as *mut u64) = new_total as u64;
        *(new_ptr as *mut u64) = new_size as u64;
    }

    // Free old buffer with its RECORDED allocation layout.
    let old_layout =
        std::alloc::Layout::from_size_align(old_total, 8).unwrap_or_else(|_| std::process::abort());
    // SAFETY: old_base/old_total are exactly the pointer and layout produced by
    // the runtime_new/resize call that allocated this array.
    unsafe {
        std::alloc::dealloc(old_base, old_layout);
    }

    new_ptr as i64
}

/// Copy `len` bytes between byte arrays: src[src_off..] -> dest[dest_off..].
/// Used by both copyByteArray# and copyMutableByteArray#.
pub extern "C" fn runtime_copy_byte_array(
    src: i64,
    src_off: i64,
    dest: i64,
    dest_off: i64,
    len: i64,
) {
    if check_ptr_invalid(src as *const u8, "runtime_copy_byte_array")
        || check_ptr_invalid(dest as *const u8, "runtime_copy_byte_array")
    {
        return;
    }
    // Before the pointer arithmetic, validate offsets
    let src_size = unsafe { *(src as *const u64) } as usize;
    let dest_size = unsafe { *(dest as *const u64) } as usize;
    if src_off < 0 || dest_off < 0 || len < 0 {
        return; // silently return for negative offsets (matches GHC behavior)
    }
    let src_off = src_off as usize;
    let dest_off = dest_off as usize;
    let len = len as usize;
    if src_off.saturating_add(len) > src_size || dest_off.saturating_add(len) > dest_size {
        return; // out of bounds
    }

    // SAFETY: src and dest passed the null-guard above. Offsetting past the 8-byte
    // length prefix + the respective offsets.
    let src_ptr = unsafe { (src as *const u8).add(8 + src_off) };
    let dest_ptr = unsafe { (dest as *mut u8).add(8 + dest_off) };
    // SAFETY: Uses copy (not copy_nonoverlapping) because src and dest may be the
    // same array with overlapping ranges.
    unsafe {
        std::ptr::copy(src_ptr, dest_ptr, len);
    }
}

/// Compare byte arrays: returns -1, 0, or 1.
pub extern "C" fn runtime_compare_byte_arrays(
    a: i64,
    a_off: i64,
    b: i64,
    b_off: i64,
    len: i64,
) -> i64 {
    if check_ptr_invalid(a as *const u8, "runtime_compare_byte_arrays")
        || check_ptr_invalid(b as *const u8, "runtime_compare_byte_arrays")
    {
        return 0;
    }
    if a_off < 0 || b_off < 0 || len < 0 {
        return 0;
    }
    let a_size = unsafe { *(a as *const u64) } as usize;
    let b_size = unsafe { *(b as *const u64) } as usize;
    if (a_off as usize).saturating_add(len as usize) > a_size
        || (b_off as usize).saturating_add(len as usize) > b_size
    {
        return 0;
    }

    // SAFETY: a and b passed the null-guard above. Offsetting past the 8-byte length
    // prefix + the respective offsets.
    let a_ptr = unsafe { (a as *const u8).add(8 + a_off as usize) };
    let b_ptr = unsafe { (b as *const u8).add(8 + b_off as usize) };
    let a_slice = unsafe { std::slice::from_raw_parts(a_ptr, len as usize) };
    let b_slice = unsafe { std::slice::from_raw_parts(b_ptr, len as usize) };
    match a_slice.cmp(b_slice) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

// ---------------------------------------------------------------------------
// Boxed array runtime functions (SmallArray# / Array#)
// ---------------------------------------------------------------------------

/// Allocate a new boxed array of `len` pointer slots, each initialized to `init`.
/// Layout: `[u64 length][ptr0][ptr1]...[ptrN-1]`
/// Each slot is 8 bytes (a heap pointer).
pub extern "C" fn runtime_new_boxed_array(len: i64, init: i64) -> i64 {
    if len < 0 {
        RUNTIME_ERROR.with(|cell| {
            *cell.borrow_mut() = Some(RuntimeError::UserErrorMsg(
                "negative length in array allocation".to_string(),
            ));
        });
        return error_poison_ptr() as i64;
    }
    let n = len as usize;
    let slot_bytes = match n.checked_mul(8) {
        Some(v) => v,
        None => {
            RUNTIME_ERROR.with(|cell| {
                *cell.borrow_mut() = Some(RuntimeError::UserErrorMsg(
                    "array size overflow".to_string(),
                ));
            });
            return error_poison_ptr() as i64;
        }
    };
    let total = match 8usize.checked_add(slot_bytes) {
        Some(v) => v,
        None => {
            RUNTIME_ERROR.with(|cell| {
                *cell.borrow_mut() = Some(RuntimeError::UserErrorMsg(
                    "array size overflow".to_string(),
                ));
            });
            return error_poison_ptr() as i64;
        }
    };
    let layout =
        std::alloc::Layout::from_size_align(total, 8).unwrap_or_else(|_| std::process::abort());
    // SAFETY: alloc returns a valid allocation of the requested size.
    let ptr = unsafe { std::alloc::alloc(layout) };
    if ptr.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
    // SAFETY: ptr is a fresh allocation of (8 + 8*n) bytes. Initializing all
    // pointer slots to `init` and then writing the length prefix.
    unsafe {
        let slots = ptr.add(8) as *mut i64;
        for i in 0..n {
            *slots.add(i) = init;
        }
        // Write length after slots are initialized so a concurrent reader
        // (e.g. GC walking) never sees a length prefix with uninit slots.
        *(ptr as *mut u64) = n as u64;
    }
    ptr as i64
}

/// Clone a sub-range of a boxed array: src[off..off+len].
pub extern "C" fn runtime_clone_boxed_array(src: i64, off: i64, len: i64) -> i64 {
    if (src as u64) < MIN_VALID_ADDR {
        return error_poison_ptr() as i64;
    }
    if len < 0 {
        RUNTIME_ERROR.with(|cell| {
            *cell.borrow_mut() = Some(RuntimeError::UserErrorMsg(
                "negative length in array allocation".to_string(),
            ));
        });
        return error_poison_ptr() as i64;
    }
    let n = len as usize;
    let slot_bytes = match n.checked_mul(8) {
        Some(v) => v,
        None => {
            RUNTIME_ERROR.with(|cell| {
                *cell.borrow_mut() = Some(RuntimeError::UserErrorMsg(
                    "array size overflow".to_string(),
                ));
            });
            return error_poison_ptr() as i64;
        }
    };
    let total = match 8usize.checked_add(slot_bytes) {
        Some(v) => v,
        None => {
            RUNTIME_ERROR.with(|cell| {
                *cell.borrow_mut() = Some(RuntimeError::UserErrorMsg(
                    "array size overflow".to_string(),
                ));
            });
            return error_poison_ptr() as i64;
        }
    };

    // Before the pointer arithmetic, validate offsets against source
    let src_n = unsafe { *(src as *const u64) } as usize;
    if off < 0 || (off as usize).saturating_add(n) > src_n {
        return error_poison_ptr() as i64; // silently return
    }

    let layout =
        std::alloc::Layout::from_size_align(total, 8).unwrap_or_else(|_| std::process::abort());
    // SAFETY: alloc returns a valid allocation of the requested size.
    let ptr = unsafe { std::alloc::alloc(layout) };
    if ptr.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
    // SAFETY: ptr is a fresh allocation. src is a valid boxed array from JIT code.
    // Copying len pointer slots from src[off..off+len] to the new array.
    unsafe {
        *(ptr as *mut u64) = n as u64;
        let src_slots = (src as *const u8).add(8 + 8 * off as usize);
        let dst_slots = ptr.add(8);
        std::ptr::copy_nonoverlapping(src_slots, dst_slots, 8 * n);
    }
    ptr as i64
}

/// Copy `len` pointer slots from src[src_off..] to dest[dest_off..].
pub extern "C" fn runtime_copy_boxed_array(
    src: i64,
    src_off: i64,
    dest: i64,
    dest_off: i64,
    len: i64,
) {
    if (src as u64) < MIN_VALID_ADDR || (dest as u64) < MIN_VALID_ADDR {
        return;
    }
    if src_off < 0 || dest_off < 0 || len < 0 {
        return;
    }
    let src_n = unsafe { *(src as *const u64) } as usize;
    let dest_n = unsafe { *(dest as *const u64) } as usize;
    let src_off = src_off as usize;
    let dest_off = dest_off as usize;
    let len = len as usize;
    if src_off.saturating_add(len) > src_n || dest_off.saturating_add(len) > dest_n {
        return; // out of bounds
    }

    // SAFETY: src and dest are valid boxed array pointers from JIT code. Offsetting
    // past the 8-byte length prefix by the slot-sized offsets. Uses copy (not
    // copy_nonoverlapping) because src and dest may be the same array.
    let src_ptr = unsafe { (src as *const u8).add(8 + 8 * src_off) };
    let dest_ptr = unsafe { (dest as *mut u8).add(8 + 8 * dest_off) };
    unsafe {
        std::ptr::copy(src_ptr, dest_ptr, 8 * len);
    }
}

/// Shrink a boxed array (just update the length field).
pub extern "C" fn runtime_shrink_boxed_array(arr: i64, new_len: i64) {
    if new_len < 0 || (arr as u64) < MIN_VALID_ADDR {
        return;
    }
    let old_len = unsafe { *(arr as *const u64) } as i64;
    if new_len > old_len {
        return; // only allow shrink, not grow
    }
    // SAFETY: arr is a valid boxed array pointer from JIT code. Writing the length
    // prefix at offset 0 with a smaller value (logical shrink).
    unsafe {
        *(arr as *mut u64) = new_len as u64;
    }
}

/// CAS on a boxed array slot: compare-and-swap `arr[idx]`.
/// Returns the old value. If old == expected, writes new.
pub extern "C" fn runtime_cas_boxed_array(arr: i64, idx: i64, expected: i64, new: i64) -> i64 {
    if (arr as u64) < MIN_VALID_ADDR || idx < 0 {
        return error_poison_ptr() as i64;
    }
    let n = unsafe { *(arr as *const u64) } as usize;
    if idx as usize >= n {
        return error_poison_ptr() as i64;
    }
    // SAFETY: arr is a valid boxed array pointer from JIT code. idx is within bounds.
    // Reading and conditionally writing a single pointer-sized slot.
    let slot = unsafe { (arr as *mut u8).add(8 + 8 * idx as usize) as *mut i64 };
    let old = unsafe { *slot };
    if old == expected {
        unsafe { *slot = new };
    }
    old
}

/// Decode a Double into its Int64 mantissa (significand).
/// GHC's `decodeDouble_Int64#` returns (# mantissa, exponent #).
pub extern "C" fn runtime_decode_double_mantissa(bits: i64) -> i64 {
    let (man, _) = decode_double_int64(f64::from_bits(bits as u64));
    man
}

/// Decode a Double into its Int exponent.
pub extern "C" fn runtime_decode_double_exponent(bits: i64) -> i64 {
    let (_, exp) = decode_double_int64(f64::from_bits(bits as u64));
    exp
}

/// Shared implementation matching GHC's `decodeDouble_Int64#` semantics.
/// Returns (mantissa, exponent) such that mantissa * 2^exponent == d,
/// with mantissa normalized to have no trailing zeros in binary.
fn decode_double_int64(d: f64) -> (i64, i64) {
    if d == 0.0 || d.is_nan() {
        return (0, 0);
    }
    if d.is_infinite() {
        return (if d > 0.0 { 1 } else { -1 }, 0);
    }
    let bits = d.to_bits();
    let sign: i64 = if bits >> 63 == 0 { 1 } else { -1 };
    let raw_exp = ((bits >> 52) & 0x7ff) as i32;
    let raw_man = (bits & 0x000f_ffff_ffff_ffff) as i64;
    let (man, exp) = if raw_exp == 0 {
        // subnormal
        (raw_man, 1 - 1023 - 52)
    } else {
        // normal: implicit leading 1
        (raw_man | (1i64 << 52), raw_exp - 1023 - 52)
    };
    let man = sign * man;
    if man != 0 {
        let tz = man.unsigned_abs().trailing_zeros();
        (man >> tz, (exp + tz as i32) as i64)
    } else {
        (0, 0)
    }
}

/// strlen: count bytes until null terminator.
pub extern "C" fn runtime_strlen(addr: i64) -> i64 {
    if check_ptr_invalid(addr as *const u8, "runtime_strlen") {
        return 0;
    }
    let ptr = addr as *const u8;
    let mut len = 0i64;
    // SAFETY: addr passed the null-guard above. The pointer is a null-terminated
    // C string from JIT data sections or unpackCString#. Scanning until '\0'.
    unsafe {
        while *ptr.add(len as usize) != 0 {
            len += 1;
        }
    }
    len
}

/// Measure codepoints in a UTF-8 buffer. Matches text-2.1.2 `_hs_text_measure_off` semantics.
///
/// If the buffer contains >= `cnt` characters, returns the non-negative byte count
/// of those `cnt` characters. If the buffer is shorter (< `cnt` chars), returns
/// the non-positive negated total character count. Returns 0 if `len` = 0 or `cnt` = 0.
///
/// # Safety
/// Input must be valid UTF-8. No validation is performed (matches C text library).
pub extern "C" fn runtime_text_measure_off(addr: i64, off: i64, len: i64, cnt: i64) -> i64 {
    if len <= 0 || cnt <= 0 {
        return 0;
    }
    if check_ptr_invalid(addr as *const u8, "runtime_text_measure_off") {
        return 0;
    }
    let ptr = (addr + off) as *const u8;
    let len = len as usize;
    let cnt = cnt as usize;
    let mut byte_pos = 0usize;
    let mut chars_found = 0usize;
    while chars_found < cnt && byte_pos < len {
        // SAFETY: byte_pos < len, so ptr + byte_pos is within the buffer.
        let b = unsafe { *ptr.add(byte_pos) };
        let char_len = if b < 0x80 {
            1
        } else if b < 0xE0 {
            2
        } else if b < 0xF0 {
            3
        } else {
            4
        };
        byte_pos += char_len;
        chars_found += 1;
    }
    if chars_found >= cnt {
        // Buffer had enough characters — return bytes consumed (non-negative)
        byte_pos as i64
    } else {
        // Buffer exhausted before cnt — return negated char count (non-positive)
        -(chars_found as i64)
    }
}

/// Find a byte in a buffer. Returns offset from start, or -1 if not found.
pub extern "C" fn runtime_text_memchr(addr: i64, off: i64, len: i64, needle: i64) -> i64 {
    if len <= 0 {
        return -1;
    }
    if check_ptr_invalid(addr as *const u8, "runtime_text_memchr") {
        return -1;
    }
    let ptr = (addr + off) as *const u8;
    // SAFETY: addr passed the null-guard above. ptr = addr + off points into a valid
    // Text buffer, and len bytes are readable from that position.
    let slice = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    match slice.iter().position(|&b| b == needle as u8) {
        Some(pos) => pos as i64,
        None => -1,
    }
}

/// Reverse UTF-8 text. Matches text-2.1.2 `_hs_text_reverse(dst0, src0, off, len)`.
///
/// Reads `len` bytes from `src + off`, writes reversed codepoints starting at `dst`.
pub extern "C" fn runtime_text_reverse(dest: i64, src: i64, off: i64, len: i64) {
    if len <= 0 {
        return;
    }
    if check_ptr_invalid(dest as *const u8, "runtime_text_reverse")
        || check_ptr_invalid(src as *const u8, "runtime_text_reverse")
    {
        return;
    }
    let src_ptr = (src + off) as *const u8;
    // SAFETY: src + off points into a valid Text buffer and len bytes are readable.
    let src_slice = unsafe { std::slice::from_raw_parts(src_ptr, len as usize) };
    let dest_ptr = dest as *mut u8;
    // Decode UTF-8 codepoints, write in reverse order
    let mut read_pos = 0usize;
    let mut write_pos = len as usize;
    while read_pos < len as usize {
        let b = src_slice[read_pos];
        let char_len = if b < 0x80 {
            1
        } else if b < 0xE0 {
            2
        } else if b < 0xF0 {
            3
        } else {
            4
        };
        write_pos -= char_len;
        // SAFETY: read_pos and write_pos are within their respective buffers.
        // src and dest do not overlap (separate allocations from JIT code).
        unsafe {
            std::ptr::copy_nonoverlapping(
                src_slice.as_ptr().add(read_pos),
                dest_ptr.add(write_pos),
                char_len,
            );
        }
        read_pos += char_len;
    }
}

/// `quotRemWord2#` quotient: `((hi << 64) | lo) / d`. The native ghc-bignum
/// backend's 128/64 division primitive. `d == 0` is guarded by the Haskell
/// caller (raiseDivZero#); we still return 0 rather than panic.
pub extern "C" fn runtime_word2_quot(hi: i64, lo: i64, d: i64) -> i64 {
    let d = d as u64;
    if d == 0 {
        return 0;
    }
    let n = ((hi as u64 as u128) << 64) | (lo as u64 as u128);
    (n / d as u128) as u64 as i64
}

/// `quotRemWord2#` remainder: `((hi << 64) | lo) % d`.
pub extern "C" fn runtime_word2_rem(hi: i64, lo: i64, d: i64) -> i64 {
    let d = d as u64;
    if d == 0 {
        return 0;
    }
    let n = ((hi as u64 as u128) << 64) | (lo as u64 as u128);
    (n % d as u128) as u64 as i64
}

/// `__int_encodeDouble(mantissa, exp) -> Double#` (returned as raw f64 bits).
pub extern "C" fn runtime_int_encode_double(mantissa: i64, exp: i64) -> i64 {
    tidepool_bignum::encode_double(mantissa, exp).to_bits() as i64
}

/// `__word_encodeDouble(mantissa, exp) -> Double#` (unsigned mantissa; raw bits).
pub extern "C" fn runtime_word_encode_double(mantissa: i64, exp: i64) -> i64 {
    tidepool_bignum::encode_double_word(mantissa as u64, exp).to_bits() as i64
}

/// Format a Double as a null-terminated C string and return its address.
/// The CString is leaked (small bounded strings, acceptable).
pub extern "C" fn runtime_show_double_addr(bits: i64) -> i64 {
    let d = f64::from_bits(bits as u64);
    let s = haskell_show_double(d);
    let c_str = match std::ffi::CString::new(s) {
        Ok(c) => c,
        Err(_) => {
            RUNTIME_ERROR.with(|cell| {
                *cell.borrow_mut() = Some(RuntimeError::Undefined);
            });
            return error_poison_ptr() as i64;
        }
    };
    let ptr = c_str.into_raw();
    ptr as i64
}

/// Format a Double matching Haskell's `show` output.
/// Decimal notation for 0.1 <= |x| < 1e7, scientific notation otherwise.
/// Always includes a decimal point.
fn haskell_show_double(d: f64) -> String {
    if d.is_nan() {
        return "NaN".to_string();
    }
    if d.is_infinite() {
        return if d > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    if d == 0.0 {
        return if d.is_sign_negative() { "-0.0" } else { "0.0" }.to_string();
    }
    let abs = d.abs();
    if (0.1..1.0e7).contains(&abs) {
        let s = d.to_string();
        if s.contains('.') {
            s
        } else {
            format!("{}.0", s)
        }
    } else {
        // Scientific notation. Haskell's `show` mantissa always carries a
        // decimal point ("1.0e10", "5.0e-324"); Rust's {:e} omits it for
        // integral mantissas ("1e10"). Insert ".0" before the exponent when
        // missing. (proptest_host_arrays BUG-1)
        let s = format!("{:e}", d);
        match s.find('e') {
            Some(epos) if !s[..epos].contains('.') => {
                format!("{}.0{}", &s[..epos], &s[epos..])
            }
            _ => s,
        }
    }
}

// --- Double math runtime functions (libm wrappers) ---
// All take f64-as-i64-bits and return f64-as-i64-bits.
macro_rules! double_math_unary {
    ($name:ident, $op:ident) => {
        pub extern "C" fn $name(bits: i64) -> i64 {
            let d = f64::from_bits(bits as u64);
            f64::$op(d).to_bits() as i64
        }
    };
}

double_math_unary!(runtime_double_exp, exp);
double_math_unary!(runtime_double_expm1, exp_m1);
double_math_unary!(runtime_double_log, ln);
double_math_unary!(runtime_double_log1p, ln_1p);
double_math_unary!(runtime_double_sin, sin);
double_math_unary!(runtime_double_cos, cos);
double_math_unary!(runtime_double_tan, tan);
double_math_unary!(runtime_double_asin, asin);
double_math_unary!(runtime_double_acos, acos);
double_math_unary!(runtime_double_atan, atan);
double_math_unary!(runtime_double_sinh, sinh);
double_math_unary!(runtime_double_cosh, cosh);
double_math_unary!(runtime_double_tanh, tanh);
double_math_unary!(runtime_double_asinh, asinh);
double_math_unary!(runtime_double_acosh, acosh);
double_math_unary!(runtime_double_atanh, atanh);

pub extern "C" fn runtime_double_power(bits_a: i64, bits_b: i64) -> i64 {
    let a = f64::from_bits(bits_a as u64);
    let b = f64::from_bits(bits_b as u64);
    a.powf(b).to_bits() as i64
}

// ---------------------------------------------------------------------------
// Lazy effect-result materialization
//
// Large list-shaped effect responses are not converted to heap cells eagerly.
// Instead the dispatcher parks the flattened elements in a thread-local
// registry and responds with a single thunk whose code pointer is the HOST
// function `lazy_list_chunk` (precedent: poison trampolines — `heap_force`
// calls thunk entries through a transmute and cannot tell host from JIT).
// Forcing the tail materializes the next CHUNK elements and a fresh tail
// thunk. `take k` over a huge glob materializes one chunk ever; full folds
// stream chunks through the (growing) heap while consumed cells become
// garbage. The captures are raw ints — GC's evacuation range-check skips
// non-pointer words, same as the vmctx tail fields.
// ---------------------------------------------------------------------------

/// A parked effect-response stream: the element producer (the iterator IS
/// the cursor — no offset bookkeeping), the list constructor tags, and an
/// owned `DataConTable` for pull-time element conversion. Sequential,
/// exactly-once consumption is guaranteed structurally: tail thunk N is
/// unreachable until tail N−1 was forced, and `heap_force` memoizes.
pub(crate) struct ParkedStream {
    pub source: Box<dyn tidepool_effect::ValueSource>,
    pub cons_tag: u64,
    pub nil_tag: u64,
    /// Conversion table for pull-time `ToCore`. Pre-converted sources
    /// (dismantled spines) ignore it — park an empty table for those.
    pub table: tidepool_repr::DataConTable,
}

thread_local! {
    static PARKED_STREAMS: RefCell<std::collections::HashMap<u64, ParkedStream>> =
        RefCell::new(std::collections::HashMap::new());
    static STREAM_NEXT_ID: Cell<u64> = const { Cell::new(1) };
}

/// Source over pre-converted Values (a dismantled `Response::Complete`
/// spine). The table argument is unused. Random-access: element values are
/// cheap to clone (Text payloads are Arc-shared), so element thunks defer
/// only the value_to_heap byte copies.
pub(crate) struct ReadySource {
    items: Vec<tidepool_eval::value::Value>,
    pos: usize,
}

impl ReadySource {
    pub(crate) fn new(items: Vec<tidepool_eval::value::Value>) -> Self {
        Self { items, pos: 0 }
    }
}

impl tidepool_effect::ValueSource for ReadySource {
    fn next_value(
        &mut self,
        _table: &tidepool_repr::DataConTable,
    ) -> Option<Result<tidepool_eval::value::Value, tidepool_bridge::BridgeError>> {
        let item = self.items.get(self.pos)?;
        self.pos += 1;
        Some(Ok(item.clone()))
    }

    fn len(&self) -> Option<usize> {
        Some(self.items.len())
    }

    fn get(
        &self,
        idx: usize,
        _table: &tidepool_repr::DataConTable,
    ) -> Option<Result<tidepool_eval::value::Value, tidepool_bridge::BridgeError>> {
        self.items.get(idx).map(|v| Ok(v.clone()))
    }
}

/// Park a response stream; returns the registry id carried by tail thunks.
pub(crate) fn park_stream(stream: ParkedStream) -> u64 {
    let id = STREAM_NEXT_ID.with(|c| {
        let v = c.get();
        c.set(v + 1);
        v
    });
    PARKED_STREAMS.with(|r| r.borrow_mut().insert(id, stream));
    id
}

/// Drop all parked streams (machine teardown).
pub(crate) fn clear_parked_streams() {
    PARKED_STREAMS.with(|r| r.borrow_mut().clear());
}

/// Nursery allocation from host code with one GC-and-retry. Any heap
/// pointers the CALLER holds across this call must be RUST_ROOTS-registered.
///
/// # Safety
/// `vmctx` must be valid with a live nursery and GC state installed.
unsafe fn host_alloc_gc(vmctx: *mut VMContext, size: usize) -> *mut u8 {
    let p = crate::heap_bridge::bump_alloc_from_vmctx(&mut *vmctx, size);
    if !p.is_null() {
        return p;
    }
    gc_trigger(vmctx);
    crate::heap_bridge::bump_alloc_from_vmctx(&mut *vmctx, size)
}

/// Allocate a host-code thunk with two raw u64 captures. Raw ints are safe
/// captures: the GC's evacuation range-check skips non-pointer words.
///
/// # Safety
/// `vmctx` must be valid with a live nursery and GC state installed.
unsafe fn alloc_host_thunk2(
    vmctx: *mut VMContext,
    code: unsafe extern "C" fn(*mut VMContext, *mut u8) -> *mut u8,
    cap0: u64,
    cap1: u64,
) -> *mut u8 {
    let size = tidepool_heap::layout::THUNK_CAPTURED_OFFSET + 16;
    let p = host_alloc_gc(vmctx, size);
    if p.is_null() {
        return std::ptr::null_mut();
    }
    tidepool_heap::layout::write_header(p, tidepool_heap::layout::TAG_THUNK, size as u16);
    *p.add(tidepool_heap::layout::THUNK_STATE_OFFSET) = tidepool_heap::layout::THUNK_UNEVALUATED;
    *(p.add(tidepool_heap::layout::THUNK_CODE_PTR_OFFSET) as *mut usize) = code as usize;
    *(p.add(tidepool_heap::layout::THUNK_CAPTURED_OFFSET) as *mut u64) = cap0;
    *(p.add(tidepool_heap::layout::THUNK_CAPTURED_OFFSET + 8) as *mut u64) = cap1;
    p
}

/// Allocate a stream-tail thunk carrying (registry id, offset). Sequential
/// sources ignore the offset (the parked iterator is the cursor); indexed
/// sources use it as the next chunk's start index.
/// Returns null on OOM (caller converts to poison via runtime_oom).
///
/// # Safety
/// `vmctx` must be valid with a live nursery and GC state installed.
pub(crate) unsafe fn alloc_stream_tail_thunk(
    vmctx: *mut VMContext,
    id: u64,
    offset: u64,
) -> *mut u8 {
    alloc_host_thunk2(vmctx, stream_chunk, id, offset)
}

/// Allocate an element thunk carrying (registry id, element index):
/// forcing it converts exactly that element of an indexed source,
/// memoized by the standard thunk indirection.
///
/// # Safety
/// `vmctx` must be valid with a live nursery and GC state installed.
unsafe fn alloc_element_thunk(vmctx: *mut VMContext, id: u64, idx: u64) -> *mut u8 {
    alloc_host_thunk2(vmctx, stream_element, id, idx)
}

/// Allocate a nullary constructor (e.g. nil). Returns null on OOM.
///
/// # Safety
/// `vmctx` must be valid with a live nursery and GC state installed.
unsafe fn alloc_nullary_con(vmctx: *mut VMContext, con_tag: u64) -> *mut u8 {
    let size = tidepool_heap::layout::CON_FIELDS_OFFSET;
    let p = host_alloc_gc(vmctx, size);
    if p.is_null() {
        return std::ptr::null_mut();
    }
    tidepool_heap::layout::write_header(p, tidepool_heap::layout::TAG_CON, size as u16);
    *(p.add(tidepool_heap::layout::CON_TAG_OFFSET) as *mut u64) = con_tag;
    *(p.add(tidepool_heap::layout::CON_NUM_FIELDS_OFFSET) as *mut u16) = 0;
    p
}

/// Build cons cells back-to-front over `items`, linking `terminator` as the
/// final tail. GC-safe: the terminator and partial chain are RUST_ROOTS
/// registered; element conversion retries once after gc_trigger. Returns the
/// chain head, or a poison pointer with a runtime error set on failure.
///
/// # Safety
/// `vmctx` must be valid with a live nursery and GC state installed;
/// `terminator` must be a valid heap pointer.
unsafe fn build_cons_cells(
    vmctx: *mut VMContext,
    cons_tag: u64,
    items: &[tidepool_eval::value::Value],
    terminator: *mut u8,
) -> *mut u8 {
    let mark = rust_roots_mark();
    let mut tail: *mut u8 = terminator;
    register_rust_root(&mut tail as *mut *mut u8);

    // Build cells back-to-front so each cons links the already-built tail.
    let mut elem: *mut u8 = std::ptr::null_mut();
    register_rust_root(&mut elem as *mut *mut u8);
    for v in items.iter().rev() {
        elem = match crate::heap_bridge::value_to_heap(v, &mut *vmctx) {
            Ok(p) => p,
            Err(crate::heap_bridge::BridgeError::NurseryExhausted) => {
                gc_trigger(vmctx);
                match crate::heap_bridge::value_to_heap(v, &mut *vmctx) {
                    Ok(p) => p,
                    Err(_) => {
                        truncate_rust_roots(mark);
                        return runtime_oom();
                    }
                }
            }
            Err(_) => {
                truncate_rust_roots(mark);
                let msg = b"effect result: element conversion failed";
                return runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64);
            }
        };
        let size = tidepool_heap::layout::CON_FIELDS_OFFSET + 16;
        let cell = host_alloc_gc(vmctx, size);
        if cell.is_null() {
            truncate_rust_roots(mark);
            return runtime_oom();
        }
        tidepool_heap::layout::write_header(cell, tidepool_heap::layout::TAG_CON, size as u16);
        *(cell.add(tidepool_heap::layout::CON_TAG_OFFSET) as *mut u64) = cons_tag;
        *(cell.add(tidepool_heap::layout::CON_NUM_FIELDS_OFFSET) as *mut u16) = 2;
        *(cell.add(tidepool_heap::layout::CON_FIELDS_OFFSET) as *mut *mut u8) = elem;
        *(cell.add(tidepool_heap::layout::CON_FIELDS_OFFSET + 8) as *mut *mut u8) = tail;
        tail = cell;
    }
    truncate_rust_roots(mark);
    tail
}

/// Eagerly materialize a whole flattened list as a heap cons chain,
/// iteratively — no recursion over the spine. (A deep spine's recursive
/// `value_to_heap` or recursive `Value` Drop overflows the host stack; the
/// fault lands outside signal protection and silently kills the eval
/// thread.) Used by the dispatch site when lazy results are disabled.
/// Returns the chain head, or a poison pointer with a runtime error set.
///
/// # Safety
/// `vmctx` must be valid with a live nursery and GC state installed.
pub(crate) unsafe fn materialize_cons_list(
    vmctx: *mut VMContext,
    cons_tag: u64,
    nil_tag: u64,
    items: &[tidepool_eval::value::Value],
) -> *mut u8 {
    let nil = alloc_nullary_con(vmctx, nil_tag);
    if nil.is_null() {
        return runtime_oom();
    }
    build_cons_cells(vmctx, cons_tag, items, nil)
}

/// Outcome of pulling one chunk from a parked stream (separated from the
/// heap work so the registry borrow is released before any allocation).
enum ChunkPull {
    Missing,
    Cancelled,
    Failed(String),
    Chunk {
        cons_tag: u64,
        nil_tag: u64,
        items: Vec<tidepool_eval::value::Value>,
        exhausted: bool,
    },
}

/// Thunk entry for stream tails: pull the next chunk of elements from the
/// parked source, materialize them as cons cells, terminated by nil or a
/// fresh tail thunk.
///
/// # Safety
/// Called by `heap_force` with a valid vmctx and a thunk allocated by
/// `alloc_stream_tail_thunk`.
unsafe extern "C" fn stream_chunk(vmctx: *mut VMContext, thunk: *mut u8) -> *mut u8 {
    const CHUNK: usize = 256;
    // Read the captures before any allocation (the thunk may move on GC; its
    // registered slot lives in heap_force's frame, not ours).
    let id = *(thunk.add(tidepool_heap::layout::THUNK_CAPTURED_OFFSET) as *const u64);
    let offset =
        *(thunk.add(tidepool_heap::layout::THUNK_CAPTURED_OFFSET + 8) as *const u64) as usize;

    // Indexed sources (random access, e.g. respond_list / dismantled
    // spines): build spine cells whose HEADS are per-element thunks —
    // forcing a head converts exactly one element; a fold that never
    // inspects heads (length) converts nothing. Registry lookups only;
    // no producer code runs, so no panic/cancel containment needed here.
    let indexed = PARKED_STREAMS.with(|r| {
        let map = r.borrow();
        map.get(&id)
            .map(|ps| (ps.source.len(), ps.cons_tag, ps.nil_tag))
    });
    let Some((src_len, cons_tag, nil_tag)) = indexed else {
        let msg = b"effect result stream: registry entry missing (stale continuation?)";
        return runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64);
    };
    if let Some(len) = src_len {
        let end = (offset + CHUNK).min(len);
        // NOTE: the registry entry must outlive the LAST element force, not
        // just the last chunk — entries for indexed sources are dropped at
        // machine teardown (RegistryGuard), never on exhaustion.
        let terminator: *mut u8 = if end >= len {
            let p = alloc_nullary_con(vmctx, nil_tag);
            if p.is_null() {
                return runtime_oom();
            }
            p
        } else {
            let p = alloc_stream_tail_thunk(vmctx, id, end as u64);
            if p.is_null() {
                return runtime_oom();
            }
            p
        };
        return build_cons_cells_thunked(vmctx, cons_tag, id, offset..end, terminator);
    }

    // Sequential sources: pull and CONVERT a chunk of elements. Runs
    // arbitrary producer Rust — two containments:
    // - catch_unwind: a producer panic must not unwind across the JIT
    //   frames below us (UB) — convert to a runtime error instead.
    // - cancel safepoint per pull: a slow (e.g. IO-backed) producer must
    //   be interruptible like any other long-running evaluation.
    // No heap operations happen while the registry borrow is held.
    let pulled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        PARKED_STREAMS.with(|r| {
            let mut map = r.borrow_mut();
            let Some(ps) = map.get_mut(&id) else {
                return ChunkPull::Missing;
            };
            let mut items = Vec::with_capacity(CHUNK);
            let mut exhausted = false;
            while items.len() < CHUNK {
                if check_cancel_and_set_error() {
                    return ChunkPull::Cancelled;
                }
                match ps.source.next_value(&ps.table) {
                    Some(Ok(v)) => items.push(v),
                    Some(Err(e)) => {
                        return ChunkPull::Failed(format!("stream element conversion failed: {e}"))
                    }
                    None => {
                        exhausted = true;
                        break;
                    }
                }
            }
            ChunkPull::Chunk {
                cons_tag: ps.cons_tag,
                nil_tag: ps.nil_tag,
                items,
                exhausted,
            }
        })
    }));

    let (cons_tag, nil_tag, items, exhausted) = match pulled {
        Ok(ChunkPull::Chunk {
            cons_tag,
            nil_tag,
            items,
            exhausted,
        }) => (cons_tag, nil_tag, items, exhausted),
        Ok(ChunkPull::Missing) => {
            let msg = b"effect result stream: registry entry missing (stale continuation?)";
            return runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64);
        }
        Ok(ChunkPull::Cancelled) => {
            // check_cancel_and_set_error already set the runtime error.
            return error_poison_ptr();
        }
        Ok(ChunkPull::Failed(msg)) => {
            push_diagnostic(msg.clone());
            return runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64);
        }
        Err(panic) => {
            let what = panic
                .downcast_ref::<String>()
                .map(|s| s.as_str())
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .unwrap_or("<non-string panic>");
            let msg = format!("effect result stream: producer panicked: {what}");
            push_diagnostic(msg.clone());
            return runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64);
        }
    };

    if exhausted {
        PARKED_STREAMS.with(|r| {
            r.borrow_mut().remove(&id);
        });
    }

    // Terminal: nil constructor or the next tail thunk.
    let terminator: *mut u8 = if exhausted {
        let p = alloc_nullary_con(vmctx, nil_tag);
        if p.is_null() {
            return runtime_oom();
        }
        p
    } else {
        let p = alloc_stream_tail_thunk(vmctx, id, 0);
        if p.is_null() {
            return runtime_oom();
        }
        p
    };
    build_cons_cells(vmctx, cons_tag, &items, terminator)
}

/// Build cons cells back-to-front whose heads are ELEMENT THUNKS over an
/// indexed source range. Only spine allocations happen here — no element
/// conversion, no byte copies.
///
/// # Safety
/// `vmctx` must be valid with a live nursery and GC state installed;
/// `terminator` must be a valid heap pointer.
unsafe fn build_cons_cells_thunked(
    vmctx: *mut VMContext,
    cons_tag: u64,
    id: u64,
    range: std::ops::Range<usize>,
    terminator: *mut u8,
) -> *mut u8 {
    let mark = rust_roots_mark();
    let mut tail: *mut u8 = terminator;
    register_rust_root(&mut tail as *mut *mut u8);

    let mut elem: *mut u8 = std::ptr::null_mut();
    register_rust_root(&mut elem as *mut *mut u8);
    for idx in range.rev() {
        elem = alloc_element_thunk(vmctx, id, idx as u64);
        if elem.is_null() {
            truncate_rust_roots(mark);
            return runtime_oom();
        }
        let size = tidepool_heap::layout::CON_FIELDS_OFFSET + 16;
        let cell = host_alloc_gc(vmctx, size);
        if cell.is_null() {
            truncate_rust_roots(mark);
            return runtime_oom();
        }
        tidepool_heap::layout::write_header(cell, tidepool_heap::layout::TAG_CON, size as u16);
        *(cell.add(tidepool_heap::layout::CON_TAG_OFFSET) as *mut u64) = cons_tag;
        *(cell.add(tidepool_heap::layout::CON_NUM_FIELDS_OFFSET) as *mut u16) = 2;
        *(cell.add(tidepool_heap::layout::CON_FIELDS_OFFSET) as *mut *mut u8) = elem;
        *(cell.add(tidepool_heap::layout::CON_FIELDS_OFFSET + 8) as *mut *mut u8) = tail;
        tail = cell;
    }
    truncate_rust_roots(mark);
    tail
}

/// Thunk entry for stream elements: convert exactly one element of an
/// indexed source. Memoization via the standard thunk indirection makes
/// this a copy-on-read view over the parked Rust data.
///
/// # Safety
/// Called by `heap_force` with a valid vmctx and a thunk allocated by
/// `alloc_element_thunk`.
unsafe extern "C" fn stream_element(vmctx: *mut VMContext, thunk: *mut u8) -> *mut u8 {
    // Read captures before any allocation (the thunk may move on GC).
    let id = *(thunk.add(tidepool_heap::layout::THUNK_CAPTURED_OFFSET) as *const u64);
    let idx = *(thunk.add(tidepool_heap::layout::THUNK_CAPTURED_OFFSET + 8) as *const u64) as usize;

    // ToCore conversion is (potentially) user code: contain panics.
    let converted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        PARKED_STREAMS.with(|r| {
            let map = r.borrow();
            map.get(&id).map(|ps| ps.source.get(idx, &ps.table))
        })
    }));

    let value = match converted {
        Ok(Some(Some(Ok(v)))) => v,
        Ok(Some(Some(Err(e)))) => {
            let msg = format!("stream element conversion failed: {e}");
            push_diagnostic(msg.clone());
            return runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64);
        }
        Ok(Some(None)) => {
            let msg = b"effect result stream: element index out of bounds";
            return runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64);
        }
        Ok(None) => {
            let msg = b"effect result stream: registry entry missing (stale continuation?)";
            return runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64);
        }
        Err(panic) => {
            let what = panic
                .downcast_ref::<String>()
                .map(|s| s.as_str())
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .unwrap_or("<non-string panic>");
            let msg = format!("effect result stream: element conversion panicked: {what}");
            push_diagnostic(msg.clone());
            return runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64);
        }
    };

    // Materialize with one GC-and-retry (value is GC-inert Rust data).
    match crate::heap_bridge::value_to_heap(&value, &mut *vmctx) {
        Ok(p) => p,
        Err(crate::heap_bridge::BridgeError::NurseryExhausted) => {
            gc_trigger(vmctx);
            match crate::heap_bridge::value_to_heap(&value, &mut *vmctx) {
                Ok(p) => p,
                Err(_) => runtime_oom(),
            }
        }
        Err(e) => {
            let msg = format!("stream element materialization failed: {e}");
            push_diagnostic(msg.clone());
            runtime_error_with_msg(2, msg.as_ptr(), msg.len() as u64)
        }
    }
}

pub fn host_fn_symbols() -> Vec<(&'static str, *const u8)> {
    vec![
        ("gc_trigger", gc_trigger as *const u8),
        ("runtime_oom", runtime_oom as *const u8),
        (
            "runtime_blackhole_trap",
            runtime_blackhole_trap as *const u8,
        ),
        (
            "runtime_bad_thunk_state_trap",
            runtime_bad_thunk_state_trap as *const u8,
        ),
        ("heap_force", heap_force as *const u8),
        ("unresolved_var_trap", unresolved_var_trap as *const u8),
        ("runtime_error", runtime_error as *const u8),
        (
            "runtime_error_with_msg",
            runtime_error_with_msg as *const u8,
        ),
        ("runtime_error_dynamic", runtime_error_dynamic as *const u8),
        ("debug_app_check", debug_app_check as *const u8),
        ("trampoline_resolve", trampoline_resolve as *const u8),
        (
            "runtime_new_byte_array",
            runtime_new_byte_array as *const u8,
        ),
        (
            "runtime_copy_addr_to_byte_array",
            runtime_copy_addr_to_byte_array as *const u8,
        ),
        (
            "runtime_set_byte_array",
            runtime_set_byte_array as *const u8,
        ),
        (
            "runtime_shrink_byte_array",
            runtime_shrink_byte_array as *const u8,
        ),
        (
            "runtime_resize_byte_array",
            runtime_resize_byte_array as *const u8,
        ),
        (
            "runtime_copy_byte_array",
            runtime_copy_byte_array as *const u8,
        ),
        (
            "runtime_compare_byte_arrays",
            runtime_compare_byte_arrays as *const u8,
        ),
        ("runtime_strlen", runtime_strlen as *const u8),
        (
            "runtime_decode_double_mantissa",
            runtime_decode_double_mantissa as *const u8,
        ),
        (
            "runtime_decode_double_exponent",
            runtime_decode_double_exponent as *const u8,
        ),
        (
            "runtime_text_measure_off",
            runtime_text_measure_off as *const u8,
        ),
        ("runtime_text_memchr", runtime_text_memchr as *const u8),
        ("runtime_text_reverse", runtime_text_reverse as *const u8),
        ("runtime_word2_quot", runtime_word2_quot as *const u8),
        ("runtime_word2_rem", runtime_word2_rem as *const u8),
        // ghc-bignum Integer->Double FFI (the only mpn-adjacent FFI under the
        // native backend; mantissa * 2^exp via tidepool-bignum).
        (
            "runtime_int_encode_double",
            runtime_int_encode_double as *const u8,
        ),
        (
            "runtime_word_encode_double",
            runtime_word_encode_double as *const u8,
        ),
        (
            "runtime_new_boxed_array",
            runtime_new_boxed_array as *const u8,
        ),
        (
            "runtime_clone_boxed_array",
            runtime_clone_boxed_array as *const u8,
        ),
        (
            "runtime_copy_boxed_array",
            runtime_copy_boxed_array as *const u8,
        ),
        (
            "runtime_shrink_boxed_array",
            runtime_shrink_boxed_array as *const u8,
        ),
        (
            "runtime_cas_boxed_array",
            runtime_cas_boxed_array as *const u8,
        ),
        ("runtime_case_trap", runtime_case_trap as *const u8),
        (
            "runtime_show_double_addr",
            runtime_show_double_addr as *const u8,
        ),
        // Double math (libm wrappers)
        ("runtime_double_exp", runtime_double_exp as *const u8),
        ("runtime_double_expm1", runtime_double_expm1 as *const u8),
        ("runtime_double_log", runtime_double_log as *const u8),
        ("runtime_double_log1p", runtime_double_log1p as *const u8),
        ("runtime_double_sin", runtime_double_sin as *const u8),
        ("runtime_double_cos", runtime_double_cos as *const u8),
        ("runtime_double_tan", runtime_double_tan as *const u8),
        ("runtime_double_asin", runtime_double_asin as *const u8),
        ("runtime_double_acos", runtime_double_acos as *const u8),
        ("runtime_double_atan", runtime_double_atan as *const u8),
        ("runtime_double_sinh", runtime_double_sinh as *const u8),
        ("runtime_double_cosh", runtime_double_cosh as *const u8),
        ("runtime_double_tanh", runtime_double_tanh as *const u8),
        ("runtime_double_asinh", runtime_double_asinh as *const u8),
        ("runtime_double_acosh", runtime_double_acosh as *const u8),
        ("runtime_double_atanh", runtime_double_atanh as *const u8),
        ("runtime_double_power", runtime_double_power as *const u8),
    ]
}

/// Debug: called instead of `trap user2` when TIDEPOOL_DEBUG_CASE is set.
/// Prints diagnostic info about the scrutinee that failed case matching.
/// `scrut_ptr` is the heap pointer to the scrutinee.
/// `num_alts` is the number of data alt tags expected.
/// `alt_tags` is a pointer to an array of expected tag u64 values.
pub extern "C" fn runtime_case_trap(
    scrut_ptr: i64,
    num_alts: i64,
    alt_tags: i64,
    fn_name_ptr: i64,
    fn_name_len: i64,
) -> *mut u8 {
    // Identify the enclosing compiled function (emit threads its name in).
    if fn_name_ptr != 0 && fn_name_len > 0 && fn_name_len < 4096 {
        // SAFETY: emit leaks a 'static str and passes its exact ptr/len.
        let name = unsafe {
            std::str::from_utf8_unchecked(std::slice::from_raw_parts(
                fn_name_ptr as *const u8,
                fn_name_len as usize,
            ))
        };
        eprintln!("[CASE TRAP] in compiled fn: {}", name);
    }
    // If a runtime error is already pending (e.g. DivisionByZero), the poison
    // value cascaded into a case expression. Return poison again instead of
    // aborting — the error flag will be detected when with_signal_protection
    // returns.
    let has_error = RUNTIME_ERROR.with(|cell| cell.borrow().is_some());
    if has_error {
        return error_poison_ptr();
    }

    let ptr = scrut_ptr as *const u8;

    // Check if the scrutinee is a lazy poison closure. If so, trigger it to set the error flag.
    if !ptr.is_null()
        // SAFETY: ptr is non-null (checked above). Reading the tag byte at offset 0.
        && unsafe { tidepool_heap::layout::read_tag(ptr) } == tidepool_heap::layout::TAG_CLOSURE
    {
        // SAFETY: ptr is a Closure (tag confirmed above). Reading code_ptr at the known offset.
        let code_ptr =
            unsafe { *(ptr.add(tidepool_heap::layout::CLOSURE_CODE_PTR_OFFSET) as *const usize) };
        if code_ptr == poison_trampoline_lazy as *const () as usize
            || code_ptr == poison_trampoline_lazy_msg as *const () as usize
        {
            // SAFETY: code_ptr is the poison trampoline function pointer. Calling it
            // with null vmctx and arg triggers the lazy error flag without side effects
            // beyond setting RUNTIME_ERROR.
            unsafe {
                let func: unsafe extern "C" fn(*mut VMContext, *mut u8, *mut u8) -> *mut u8 =
                    std::mem::transmute(code_ptr);
                func(std::ptr::null_mut(), ptr as *mut u8, std::ptr::null_mut());
            }
            return error_poison_ptr();
        }
    }

    use std::io::Write;
    if check_ptr_invalid(scrut_ptr as *const u8, "runtime_case_trap") {
        return error_poison_ptr();
    }
    // SAFETY: ptr passed the null/low-address guard above. Reading the tag byte at offset 0.
    let tag_byte = unsafe { *ptr };
    let tag_name = match tag_byte {
        0 => "Closure",
        1 => "Thunk",
        2 => "Con",
        3 => "Lit",
        0xFF => "Forwarded(GC bug!)",
        _ => "UNKNOWN",
    };

    // Read expected alt tags
    // SAFETY: alt_tags points to a JIT data section array of num_alts u64 tag values.
    let expected: Vec<u64> = if num_alts > 0 && alt_tags != 0 {
        (0..num_alts as usize)
            .map(|i| unsafe { *((alt_tags as *const u64).add(i)) })
            .collect()
    } else {
        vec![]
    };

    // Dump raw bytes for any object type
    // SAFETY: ptr points to a heap object. Reading 32 bytes for diagnostic dump.
    // Heap objects are always at least this size (minimum header is 8 bytes + fields).
    let raw_bytes: Vec<u8> = (0..32).map(|i| unsafe { *ptr.add(i) }).collect();
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "[CASE TRAP] raw bytes: {:02x?}", raw_bytes);

    if tag_byte == layout::TAG_CON {
        // SAFETY: tag_byte == TAG_CON confirms Con; reading con_tag and num_fields at known offsets.
        let con_tag = unsafe { *(ptr.add(layout::CON_TAG_OFFSET as usize) as *const u64) };
        let num_fields =
            unsafe { *(ptr.add(layout::CON_NUM_FIELDS_OFFSET as usize) as *const u16) };
        let _ = writeln!(
            stderr,
            "[CASE TRAP] Con: con_tag={:#x}, num_fields={}, expected_tags={:?}",
            con_tag, num_fields, expected
        );
    } else if tag_byte == layout::TAG_LIT {
        // SAFETY: tag_byte == TAG_LIT confirms Lit; reading lit_tag and value at known offsets.
        let lit_tag = unsafe { *(ptr.add(layout::LIT_TAG_OFFSET as usize) as *const u64) };
        let value = unsafe { *(ptr.add(layout::LIT_VALUE_OFFSET as usize) as *const u64) };
        let _ = writeln!(
            stderr,
            "[CASE TRAP] Lit: lit_tag={:#x}, value={:#x}, expected_tags={:?}",
            lit_tag, value, expected
        );
    } else if tag_byte == layout::TAG_CLOSURE {
        // SAFETY: tag_byte == TAG_CLOSURE confirms Closure; reading code_ptr and num_captured at known offsets.
        let code_ptr =
            unsafe { *(ptr.add(layout::CLOSURE_CODE_PTR_OFFSET as usize) as *const u64) };
        let num_captured =
            unsafe { *(ptr.add(layout::CLOSURE_NUM_CAPTURED_OFFSET as usize) as *const u16) };
        let _ = writeln!(
            stderr,
            "[CASE TRAP] Closure: code_ptr={:#x}, num_captured={}, expected_tags={:?}",
            code_ptr, num_captured, expected
        );
    } else {
        let _ = writeln!(
            stderr,
            "[CASE TRAP] tag_byte={} ({}), expected_tags={:?}",
            tag_byte, tag_name, expected
        );
    }
    let _ = stderr.flush();
    drop(stderr);
    RUNTIME_ERROR.with(|cell| {
        *cell.borrow_mut() = Some(RuntimeError::CaseTrap);
    });
    error_poison_ptr()
}

#[cfg(test)]
#[allow(clippy::approx_constant)] // tests use 3.14 literal floats as round-trip data
mod tests {
    // SAFETY: All unsafe blocks in tests operate on allocations created within
    // the test via runtime_new_byte_array or stack-allocated buffers with known
    // sizes and layouts. Pointers and offsets are controlled by the test code.
    use super::*;
    use std::alloc::{dealloc, Layout};

    // SAFETY: ptr was allocated by runtime_new_byte_array with layout [8 + size, align 8].
    unsafe fn free_byte_array(ptr: i64) {
        // Mirror the capacity-word-below-pointer scheme: the TRUE allocation
        // size lives at ba - 8 and is immune to logical shrinks.
        let base = (ptr as *mut u8).sub(BYTE_ARRAY_BASE_OFFSET);
        let total = *(base as *const u64) as usize;
        let layout = Layout::from_size_align(total, 8).unwrap();
        dealloc(base, layout);
    }

    #[test]
    fn test_runtime_new_byte_array() {
        unsafe {
            let ba = runtime_new_byte_array(10);
            assert_ne!(ba, 0);
            assert_eq!(*(ba as *const u64), 10);
            let bytes = std::slice::from_raw_parts((ba as *const u8).add(8), 10);
            assert!(bytes.iter().all(|&b| b == 0));
            free_byte_array(ba);
        }
    }

    #[test]
    fn test_runtime_copy_addr_to_byte_array() {
        unsafe {
            let ba = runtime_new_byte_array(10);
            let src = b"hello";
            runtime_copy_addr_to_byte_array(src.as_ptr() as i64, ba, 2, 5);
            let bytes = std::slice::from_raw_parts((ba as *const u8).add(8), 10);
            assert_eq!(&bytes[2..7], b"hello");
            assert_eq!(bytes[0], 0);
            assert_eq!(bytes[1], 0);
            assert_eq!(bytes[7], 0);
            free_byte_array(ba);
        }
    }

    #[test]
    fn test_runtime_set_byte_array() {
        unsafe {
            let ba = runtime_new_byte_array(10);
            runtime_set_byte_array(ba, 3, 4, 0xFF);
            let bytes = std::slice::from_raw_parts((ba as *const u8).add(8), 10);
            assert_eq!(bytes[2], 0);
            assert_eq!(bytes[3], 0xFF);
            assert_eq!(bytes[4], 0xFF);
            assert_eq!(bytes[5], 0xFF);
            assert_eq!(bytes[6], 0xFF);
            assert_eq!(bytes[7], 0);
            free_byte_array(ba);
        }
    }

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

    #[test]
    fn test_runtime_shrink_byte_array() {
        unsafe {
            let ba = runtime_new_byte_array(10);
            runtime_shrink_byte_array(ba, 5);
            assert_eq!(*(ba as *const u64), 5);
            // Logical shrink only: the capacity word below the pointer still
            // records the original allocation, so free_byte_array (and
            // runtime_resize_byte_array) dealloc with the true layout.
            free_byte_array(ba);
        }
    }

    /// BUG-2 regression (proptest_host_arrays): shrink-then-resize must
    /// dealloc the old buffer with its TRUE allocation layout (capacity
    /// word), not one derived from the shrunken logical prefix.
    #[test]
    fn test_shrink_then_resize_uses_true_layout() {
        unsafe {
            let ba = runtime_new_byte_array(64);
            for i in 0..64u8 {
                runtime_set_byte_array(ba, i as i64, 1, i as i64);
            }
            runtime_shrink_byte_array(ba, 5);
            // Old code derived the dealloc layout from the logical prefix (5)
            // here — UB. With the capacity word this deallocs 16+64 correctly.
            let resized = runtime_resize_byte_array(ba, 128);
            assert_eq!(*(resized as *const u64), 128);
            // Logical content (first 5 bytes) preserved; grown tail zeroed.
            for i in 0..5u8 {
                assert_eq!(*((resized as *const u8).add(8 + i as usize)), i);
            }
            assert_eq!(*((resized as *const u8).add(8 + 127)), 0);
            free_byte_array(resized);
        }
    }

    #[test]
    fn test_runtime_resize_byte_array_grow() {
        unsafe {
            let ba = runtime_new_byte_array(5);
            let bytes = std::slice::from_raw_parts_mut((ba as *mut u8).add(8), 5);
            bytes.copy_from_slice(b"abcde");

            let new_ba = runtime_resize_byte_array(ba, 10);
            assert_eq!(*(new_ba as *const u64), 10);
            let new_bytes = std::slice::from_raw_parts((new_ba as *const u8).add(8), 10);
            assert_eq!(&new_bytes[0..5], b"abcde");
            assert_eq!(&new_bytes[5..10], &[0, 0, 0, 0, 0]);

            free_byte_array(new_ba);
        }
    }

    #[test]
    fn test_runtime_resize_byte_array_shrink() {
        unsafe {
            let ba = runtime_new_byte_array(10);
            let bytes = std::slice::from_raw_parts_mut((ba as *mut u8).add(8), 10);
            bytes.copy_from_slice(b"0123456789");

            let new_ba = runtime_resize_byte_array(ba, 5);
            assert_eq!(*(new_ba as *const u64), 5);
            let new_bytes = std::slice::from_raw_parts((new_ba as *const u8).add(8), 5);
            assert_eq!(new_bytes, b"01234");

            free_byte_array(new_ba);
        }
    }

    #[test]
    fn test_runtime_copy_byte_array() {
        unsafe {
            let ba1 = runtime_new_byte_array(10);
            let ba2 = runtime_new_byte_array(10);

            let bytes1 = std::slice::from_raw_parts_mut((ba1 as *mut u8).add(8), 10);
            bytes1.copy_from_slice(b"abcdefghij");

            runtime_copy_byte_array(ba1, 2, ba2, 4, 3);

            let bytes2 = std::slice::from_raw_parts((ba2 as *const u8).add(8), 10);
            assert_eq!(&bytes2[4..7], b"cde");

            free_byte_array(ba1);
            free_byte_array(ba2);
        }
    }

    #[test]
    fn test_runtime_copy_byte_array_overlap() {
        unsafe {
            let ba = runtime_new_byte_array(10);
            let bytes = std::slice::from_raw_parts_mut((ba as *mut u8).add(8), 10);
            bytes.copy_from_slice(b"0123456789");

            // Overlapping copy: 01234 -> 23456
            runtime_copy_byte_array(ba, 0, ba, 2, 5);

            assert_eq!(bytes, b"0101234789");

            free_byte_array(ba);
        }
    }

    #[test]
    fn test_runtime_compare_byte_arrays() {
        unsafe {
            let ba1 = runtime_new_byte_array(5);
            let ba2 = runtime_new_byte_array(5);

            std::ptr::copy_nonoverlapping(b"apple".as_ptr(), (ba1 as *mut u8).add(8), 5);
            std::ptr::copy_nonoverlapping(b"apply".as_ptr(), (ba2 as *mut u8).add(8), 5);

            assert_eq!(runtime_compare_byte_arrays(ba1, 0, ba2, 0, 4), 0); // "appl" == "appl"
            assert_eq!(runtime_compare_byte_arrays(ba1, 0, ba2, 0, 5), -1); // "apple" < "apply"
            assert_eq!(runtime_compare_byte_arrays(ba2, 0, ba1, 0, 5), 1); // "apply" > "apple"

            free_byte_array(ba1);
            free_byte_array(ba2);
        }
    }

    #[test]
    fn test_runtime_strlen() {
        let s = b"hello\0world\0";
        unsafe {
            assert_eq!(runtime_strlen(s.as_ptr() as i64), 5);
            assert_eq!(runtime_strlen(s.as_ptr().add(6) as i64), 5);
        }
    }

    // ---------------------------------------------------------------
    // runtime_text_measure_off — text-2.1.2 semantics:
    //   cnt reached => return bytes consumed (non-negative)
    //   buffer exhausted => return -(chars_found) (non-positive)
    // ---------------------------------------------------------------

    #[test]
    fn test_measure_off_ascii_length() {
        // T.length "hello" = negate(measure_off(p, 0, 5, maxBound))
        let s = b"hello";
        let r = runtime_text_measure_off(s.as_ptr() as i64, 0, 5, i64::MAX);
        assert_eq!(r, -5); // buffer exhausted, 5 chars found
    }

    #[test]
    fn test_measure_off_ascii_take() {
        // T.take 3 "hello" => measure_off(p, 0, 5, 3)
        let s = b"hello";
        let r = runtime_text_measure_off(s.as_ptr() as i64, 0, 5, 3);
        assert_eq!(r, 3); // 3 chars = 3 bytes consumed
    }

    #[test]
    fn test_measure_off_ascii_take_all() {
        // T.take 5 "hello" => cnt == total chars, returns bytes consumed
        let s = b"hello";
        let r = runtime_text_measure_off(s.as_ptr() as i64, 0, 5, 5);
        assert_eq!(r, 5); // exactly 5 chars fit
    }

    #[test]
    fn test_measure_off_ascii_take_more() {
        // T.take 10 "hello" => cnt > total chars, buffer exhausted
        let s = b"hello";
        let r = runtime_text_measure_off(s.as_ptr() as i64, 0, 5, 10);
        assert_eq!(r, -5); // only 5 chars available
    }

    #[test]
    fn test_measure_off_ascii_drop() {
        // T.drop 2 "hello" => measure_off(p, 0, 5, 2) = 2 bytes
        let s = b"hello";
        let r = runtime_text_measure_off(s.as_ptr() as i64, 0, 5, 2);
        assert_eq!(r, 2);
    }

    #[test]
    fn test_measure_off_with_offset() {
        // Text with off=2, len=3 (substring "llo")
        let s = b"hello";
        let r = runtime_text_measure_off(s.as_ptr() as i64, 2, 3, i64::MAX);
        assert_eq!(r, -3); // 3 chars in "llo"
    }

    #[test]
    fn test_measure_off_empty() {
        let s = b"hello";
        assert_eq!(runtime_text_measure_off(s.as_ptr() as i64, 0, 0, 5), 0);
        assert_eq!(runtime_text_measure_off(s.as_ptr() as i64, 0, 5, 0), 0);
    }

    #[test]
    fn test_measure_off_utf8_length() {
        // "café" = [63 61 66 C3 A9] = 5 bytes, 4 chars
        let s = "café".as_bytes();
        assert_eq!(s.len(), 5);
        let r = runtime_text_measure_off(s.as_ptr() as i64, 0, 5, i64::MAX);
        assert_eq!(r, -4); // 4 codepoints
    }

    #[test]
    fn test_measure_off_utf8_take() {
        // T.take 3 "café" => first 3 chars = "caf" = 3 bytes
        let s = "café".as_bytes();
        let r = runtime_text_measure_off(s.as_ptr() as i64, 0, 5, 3);
        assert_eq!(r, 3); // 3 ASCII chars = 3 bytes
    }

    #[test]
    fn test_measure_off_utf8_take_past_multibyte() {
        // T.take 4 "café" => all 4 chars, 5 bytes. cnt == total, buffer exhausted
        let s = "café".as_bytes();
        let r = runtime_text_measure_off(s.as_ptr() as i64, 0, 5, 4);
        // cnt=4, walk: c(1)+a(1)+f(1)+é(2) = 5 bytes, 4 chars found, chars_found==cnt
        assert_eq!(r, 5); // bytes consumed
    }

    #[test]
    fn test_measure_off_multibyte_chars() {
        // "λ😀x" = [CE BB | F0 9F 98 80 | 78] = 7 bytes, 3 chars
        let s = "λ😀x".as_bytes();
        assert_eq!(s.len(), 7);
        // length
        assert_eq!(
            runtime_text_measure_off(s.as_ptr() as i64, 0, 7, i64::MAX),
            -3
        );
        // take 1 = "λ" = 2 bytes
        assert_eq!(runtime_text_measure_off(s.as_ptr() as i64, 0, 7, 1), 2);
        // take 2 = "λ😀" = 6 bytes
        assert_eq!(runtime_text_measure_off(s.as_ptr() as i64, 0, 7, 2), 6);
        // with offset 2 (past "λ"), len 5: "😀x" = 2 chars
        assert_eq!(
            runtime_text_measure_off(s.as_ptr() as i64, 2, 5, i64::MAX),
            -2
        );
        // take 1 from offset 2: "😀" = 4 bytes
        assert_eq!(runtime_text_measure_off(s.as_ptr() as i64, 2, 5, 1), 4);
    }

    #[test]
    fn test_measure_off_all_widths() {
        // "Aλ文😀" = 1+2+3+4 = 10 bytes, 4 chars
        let s = "Aλ文😀".as_bytes();
        assert_eq!(s.len(), 10);
        assert_eq!(
            runtime_text_measure_off(s.as_ptr() as i64, 0, 10, i64::MAX),
            -4
        );
        assert_eq!(runtime_text_measure_off(s.as_ptr() as i64, 0, 10, 1), 1); // "A"
        assert_eq!(runtime_text_measure_off(s.as_ptr() as i64, 0, 10, 2), 3); // "Aλ"
        assert_eq!(runtime_text_measure_off(s.as_ptr() as i64, 0, 10, 3), 6); // "Aλ文"
        assert_eq!(runtime_text_measure_off(s.as_ptr() as i64, 0, 10, 4), 10); // all
                                                                               // from offset 1 (past "A"), len 9: "λ文😀"
        assert_eq!(runtime_text_measure_off(s.as_ptr() as i64, 1, 9, 2), 5); // "λ文" = 2+3
    }

    #[test]
    fn test_runtime_text_memchr() {
        let s = b"abacaba";
        assert_eq!(runtime_text_memchr(s.as_ptr() as i64, 0, 7, b'a' as i64), 0);
        assert_eq!(runtime_text_memchr(s.as_ptr() as i64, 1, 6, b'a' as i64), 1); // 'a' at index 2 of original, which is offset 1 from s+1
        assert_eq!(
            runtime_text_memchr(s.as_ptr() as i64, 0, 7, b'z' as i64),
            -1
        );
    }

    // ---------------------------------------------------------------
    // runtime_text_reverse — text-2.1.2: reverse(dst, src, off, len)
    // ---------------------------------------------------------------

    #[test]
    fn test_reverse_ascii() {
        let src = b"hello";
        let mut dest = [0u8; 5];
        runtime_text_reverse(dest.as_mut_ptr() as i64, src.as_ptr() as i64, 0, 5);
        assert_eq!(&dest, b"olleh");
    }

    #[test]
    fn test_reverse_ascii_with_offset() {
        // src = "XXhello", off=2, len=5 → reverse "hello" → "olleh"
        let src = b"XXhello";
        let mut dest = [0u8; 5];
        runtime_text_reverse(dest.as_mut_ptr() as i64, src.as_ptr() as i64, 2, 5);
        assert_eq!(&dest, b"olleh");
    }

    #[test]
    fn test_reverse_utf8() {
        // "λ😀" -> CE BB | F0 9F 98 80 (6 bytes)
        // Reversed should be "😀λ" -> F0 9F 98 80 | CE BB
        let src = "λ😀".as_bytes();
        let mut dest = [0u8; 6];
        runtime_text_reverse(dest.as_mut_ptr() as i64, src.as_ptr() as i64, 0, 6);
        assert_eq!(std::str::from_utf8(&dest).unwrap(), "😀λ");
    }

    #[test]
    fn test_reverse_all_widths() {
        // "Aλ文😀" = 10 bytes → "😀文λA"
        let src = "Aλ文😀".as_bytes();
        let mut dest = [0u8; 10];
        runtime_text_reverse(dest.as_mut_ptr() as i64, src.as_ptr() as i64, 0, 10);
        assert_eq!(std::str::from_utf8(&dest).unwrap(), "😀文λA");
    }

    #[test]
    fn test_reverse_single_char() {
        let src = b"x";
        let mut dest = [0u8; 1];
        runtime_text_reverse(dest.as_mut_ptr() as i64, src.as_ptr() as i64, 0, 1);
        assert_eq!(&dest, b"x");
    }

    // ---------------------------------------------------------------
    // runtime_text_memchr — memchr(arr, off, len, byte) -> offset or -1
    // ---------------------------------------------------------------

    #[test]
    fn test_memchr_found() {
        let s = b"hello:world";
        assert_eq!(
            runtime_text_memchr(s.as_ptr() as i64, 0, 11, b':' as i64),
            5
        );
    }

    #[test]
    fn test_memchr_not_found() {
        let s = b"hello";
        assert_eq!(
            runtime_text_memchr(s.as_ptr() as i64, 0, 5, b':' as i64),
            -1
        );
    }

    #[test]
    fn test_memchr_with_offset() {
        let s = b"a:b:c";
        // search from offset 2 (past "a:"), len 3 ("b:c")
        assert_eq!(runtime_text_memchr(s.as_ptr() as i64, 2, 3, b':' as i64), 1);
    }

    #[test]
    fn test_memchr_first_byte() {
        let s = b":hello";
        assert_eq!(runtime_text_memchr(s.as_ptr() as i64, 0, 6, b':' as i64), 0);
    }

    #[test]
    fn test_memchr_last_byte() {
        let s = b"hello:";
        assert_eq!(runtime_text_memchr(s.as_ptr() as i64, 0, 6, b':' as i64), 5);
    }

    // ---------------------------------------------------------------
    // decode_double_int64 — matches GHC's decodeDouble_Int64#
    // ---------------------------------------------------------------

    #[test]
    fn test_decode_double_3_14() {
        let (m, e) = decode_double_int64(3.14);
        assert_eq!(m as f64 * (2.0f64).powi(e as i32), 3.14);
    }

    #[test]
    fn test_decode_double_1_0() {
        let (m, e) = decode_double_int64(1.0);
        assert_eq!((m, e), (1, 0));
    }

    #[test]
    fn test_decode_double_42_0() {
        let (m, e) = decode_double_int64(42.0);
        assert_eq!(m as f64 * (2.0f64).powi(e as i32), 42.0);
    }

    #[test]
    fn test_decode_double_zero() {
        assert_eq!(decode_double_int64(0.0), (0, 0));
    }

    #[test]
    fn test_decode_double_negative() {
        let (m, e) = decode_double_int64(-1.5);
        assert_eq!((m, e), (-3, -1));
    }

    #[test]
    fn test_decode_double_runtime_mantissa() {
        let bits = 3.14f64.to_bits() as i64;
        let m = runtime_decode_double_mantissa(bits);
        let e = runtime_decode_double_exponent(bits);
        assert_eq!(m as f64 * (2.0f64).powi(e as i32), 3.14);
    }

    #[test]
    fn test_diagnostics() {
        let _ = drain_diagnostics();
        push_diagnostic("test1".to_string());
        push_diagnostic("test2".to_string());
        let d = drain_diagnostics();
        assert_eq!(d, vec!["test1".to_string(), "test2".to_string()]);
        let d2 = drain_diagnostics();
        assert!(d2.is_empty());
    }

    extern "C" fn mock_gc_trigger(_vmctx: *mut VMContext) {}

    thread_local! {
        static TEST_RESULT: Cell<*mut u8> = const { Cell::new(std::ptr::null_mut()) };
    }

    // SAFETY: Test-only mock thunk entry. Returns a pre-set pointer from thread-local storage.
    unsafe extern "C" fn test_thunk_entry(_vmctx: *mut VMContext, _thunk: *mut u8) -> *mut u8 {
        TEST_RESULT.with(|r| r.get())
    }

    #[test]
    fn test_heap_force_thunk_unevaluated() {
        unsafe {
            let mut vmctx = VMContext {
                alloc_ptr: std::ptr::null_mut(),
                alloc_limit: std::ptr::null_mut(),
                gc_trigger: mock_gc_trigger,
                tail_callee: std::ptr::null_mut(),
                tail_arg: std::ptr::null_mut(),
            };

            // 1. Allocate a Lit object for the result
            let mut lit_buf = [0u8; heap_layout::LIT_SIZE];
            let lit_ptr = lit_buf.as_mut_ptr();
            heap_layout::write_header(lit_ptr, layout::TAG_LIT, heap_layout::LIT_SIZE as u16);
            *(lit_ptr.add(layout::LIT_TAG_OFFSET as usize)) = 0; // Int#
            *(lit_ptr.add(layout::LIT_VALUE_OFFSET as usize) as *mut i64) = 42;

            // 2. Allocate a thunk object
            let mut thunk_buf = [0u8; layout::THUNK_MIN_SIZE as usize];
            let thunk_ptr = thunk_buf.as_mut_ptr();
            heap_layout::write_header(thunk_ptr, layout::TAG_THUNK, layout::THUNK_MIN_SIZE as u16);
            *(thunk_ptr.add(layout::THUNK_STATE_OFFSET as usize)) = layout::THUNK_UNEVALUATED;

            TEST_RESULT.with(|r| r.set(lit_ptr));
            *(thunk_ptr.add(layout::THUNK_CODE_PTR_OFFSET as usize) as *mut usize) =
                test_thunk_entry as *const () as usize;

            let res = heap_force(&mut vmctx, thunk_ptr);
            assert_eq!(res, lit_ptr);
            assert_eq!(
                *(thunk_ptr.add(layout::THUNK_STATE_OFFSET as usize)),
                layout::THUNK_EVALUATED
            );
            assert_eq!(
                *(thunk_ptr.add(layout::THUNK_INDIRECTION_OFFSET as usize) as *const *mut u8),
                lit_ptr
            );
        }
    }

    #[test]
    fn test_heap_force_thunk_evaluated() {
        unsafe {
            let mut vmctx = VMContext {
                alloc_ptr: std::ptr::null_mut(),
                alloc_limit: std::ptr::null_mut(),
                gc_trigger: mock_gc_trigger,
                tail_callee: std::ptr::null_mut(),
                tail_arg: std::ptr::null_mut(),
            };

            // 1. Result: a real heap object (Lit) so the force loop can read its tag
            let mut lit_buf = [0u8; 32];
            let lit_ptr = lit_buf.as_mut_ptr();
            heap_layout::write_header(lit_ptr, layout::TAG_LIT, 32);

            // 2. Already evaluated thunk pointing to that Lit
            let mut thunk_buf = [0u8; layout::THUNK_MIN_SIZE as usize];
            let thunk_ptr = thunk_buf.as_mut_ptr();
            heap_layout::write_header(thunk_ptr, layout::TAG_THUNK, layout::THUNK_MIN_SIZE as u16);
            *(thunk_ptr.add(layout::THUNK_STATE_OFFSET as usize)) = layout::THUNK_EVALUATED;
            *(thunk_ptr.add(layout::THUNK_INDIRECTION_OFFSET as usize) as *mut *mut u8) = lit_ptr;

            let res = heap_force(&mut vmctx, thunk_ptr);
            assert_eq!(res, lit_ptr);
        }
    }

    #[test]
    fn test_heap_force_thunk_blackhole() {
        unsafe {
            let mut vmctx = VMContext {
                alloc_ptr: std::ptr::null_mut(),
                alloc_limit: std::ptr::null_mut(),
                gc_trigger: mock_gc_trigger,
                tail_callee: std::ptr::null_mut(),
                tail_arg: std::ptr::null_mut(),
            };

            // Reset runtime error
            RUNTIME_ERROR.with(|cell| *cell.borrow_mut() = None);

            // Blackholed thunk
            let mut thunk_buf = [0u8; layout::THUNK_MIN_SIZE as usize];
            let thunk_ptr = thunk_buf.as_mut_ptr();
            heap_layout::write_header(thunk_ptr, layout::TAG_THUNK, layout::THUNK_MIN_SIZE as u16);
            *(thunk_ptr.add(layout::THUNK_STATE_OFFSET as usize)) = layout::THUNK_BLACKHOLE;

            let res = heap_force(&mut vmctx, thunk_ptr);
            // Result should be the poison object
            assert_eq!(res, error_poison_ptr());

            let err = take_runtime_error().expect("Should have flagged error");
            assert!(matches!(err, RuntimeError::BlackHole));
        }
    }

    #[test]
    fn test_heap_force_thunk_null_code_ptr() {
        unsafe {
            let mut vmctx = VMContext {
                alloc_ptr: std::ptr::null_mut(),
                alloc_limit: std::ptr::null_mut(),
                gc_trigger: mock_gc_trigger,
                tail_callee: std::ptr::null_mut(),
                tail_arg: std::ptr::null_mut(),
            };

            RUNTIME_ERROR.with(|cell| *cell.borrow_mut() = None);

            let mut thunk_buf = [0u8; layout::THUNK_MIN_SIZE as usize];
            let thunk_ptr = thunk_buf.as_mut_ptr();
            heap_layout::write_header(thunk_ptr, layout::TAG_THUNK, layout::THUNK_MIN_SIZE as u16);
            *(thunk_ptr.add(layout::THUNK_STATE_OFFSET as usize)) = layout::THUNK_UNEVALUATED;
            *(thunk_ptr.add(layout::THUNK_CODE_PTR_OFFSET as usize) as *mut usize) = 0;

            let res = heap_force(&mut vmctx, thunk_ptr);
            assert_eq!(res, error_poison_ptr());
            let err = take_runtime_error().expect("Should have flagged error");
            assert!(matches!(err, RuntimeError::NullFunPtr));
        }
    }

    #[test]
    fn test_heap_force_thunk_bad_state() {
        unsafe {
            let mut vmctx = VMContext {
                alloc_ptr: std::ptr::null_mut(),
                alloc_limit: std::ptr::null_mut(),
                gc_trigger: mock_gc_trigger,
                tail_callee: std::ptr::null_mut(),
                tail_arg: std::ptr::null_mut(),
            };

            RUNTIME_ERROR.with(|cell| *cell.borrow_mut() = None);

            let mut thunk_buf = [0u8; layout::THUNK_MIN_SIZE as usize];
            let thunk_ptr = thunk_buf.as_mut_ptr();
            heap_layout::write_header(thunk_ptr, layout::TAG_THUNK, layout::THUNK_MIN_SIZE as u16);
            *(thunk_ptr.add(layout::THUNK_STATE_OFFSET as usize)) = 255; // Invalid state

            let res = heap_force(&mut vmctx, thunk_ptr);
            assert_eq!(res, error_poison_ptr());
            let err = take_runtime_error().expect("Should have flagged error");
            assert!(matches!(err, RuntimeError::BadThunkState(255)));
        }
    }

    /// Regression test for the poison-buffer undersize bug.
    ///
    /// Prior to the fix, `runtime_oom` returned a 24-byte poison buffer that
    /// the JIT's slow-fail alloc path then treated as freshly-allocated
    /// scratch. For any Con with `>= 1` field (size `>= 32`) the post-OOM
    /// field write spilled past the 24-byte allocation into adjacent heap,
    /// manifesting as glibc "corrupted size vs. prev_size" aborts.
    ///
    /// The fix enlarges the poison buffer to absorb the maximum Con/Closure
    /// footprint the JIT can emit. This test simulates the JIT's write
    /// sequence directly: allocate a worst-case Con (24 + 1024*8 = 8216
    /// bytes) into the poison and verify no OOB writes occur.
    ///
    /// Under Miri / ASan this would fail before the fix; under glibc the
    /// corruption is non-deterministic, but the write itself is unsound
    /// and the buffer-size assertion below guards against regression.
    #[test]
    fn poison_buf_absorbs_max_con_write() {
        // The read-side decoder cap; the compile-time assertion above
        // guarantees POISON_BUF_SIZE absorbs this. The runtime check here
        // additionally exercises the full write sequence to surface any
        // overflow under Miri / ASan, not just the size relationship.
        use crate::heap_bridge::MAX_FIELDS;
        let worst_case_con = layout::CON_FIELDS_OFFSET as usize + MAX_FIELDS * 8;
        assert!(
            POISON_BUF_SIZE >= worst_case_con,
            "poison buffer ({} B) must cover worst-case Con footprint ({} B)",
            POISON_BUF_SIZE,
            worst_case_con,
        );

        // Simulate the JIT's post-OOM write sequence exactly as
        // `emit_alloc_fast_path` + the Con emitter do: tag at 0, size
        // halfword at 1, CON_TAG at 8, num_fields at 16, fields from 24.
        let ptr = runtime_oom();
        assert!(!ptr.is_null());

        // SAFETY: `ptr` is the poison buffer (POISON_BUF_SIZE >= worst_case_con).
        // Writing a TAG_CON header and MAX_FIELDS u64 field slots into it
        // stays entirely within the allocation after the fix.
        // JIT stores use `MemFlags::trusted()` which permits unaligned
        // access; mirror that with `write_unaligned` so the test also works
        // on targets where a naked deref would trap on misalignment (the
        // size halfword lands at offset 1).
        unsafe {
            ptr.write(layout::TAG_CON);
            (ptr.add(1) as *mut u16).write_unaligned(worst_case_con as u16);
            (ptr.add(layout::CON_TAG_OFFSET as usize) as *mut u64).write_unaligned(7);
            (ptr.add(layout::CON_NUM_FIELDS_OFFSET as usize) as *mut u16)
                .write_unaligned(MAX_FIELDS as u16);
            for i in 0..MAX_FIELDS {
                let off = layout::CON_FIELDS_OFFSET as usize + 8 * i;
                (ptr.add(off) as *mut u64).write_unaligned(0xDEAD_BEEF_0000_0000 | (i as u64));
            }
            // Read back a sentinel to ensure the writes landed (and weren't
            // silently dropped) — also defeats the optimizer.
            let last_off = layout::CON_FIELDS_OFFSET as usize + 8 * (MAX_FIELDS - 1);
            assert_eq!(
                (ptr.add(last_off) as *const u64).read_unaligned(),
                0xDEAD_BEEF_0000_0000 | (MAX_FIELDS as u64 - 1),
            );
        }

        // `runtime_oom` sets `RuntimeError::HeapOverflow` — clear it so
        // we don't leak state to other tests sharing this thread.
        let err = take_runtime_error().expect("runtime_oom must flag an error");
        assert!(matches!(err, RuntimeError::HeapOverflow));
    }
}
