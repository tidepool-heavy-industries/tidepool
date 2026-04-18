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
#[derive(Debug, Clone, thiserror::Error)]
pub enum RuntimeError {
    #[error("division by zero")]
    DivisionByZero,
    #[error("arithmetic overflow")]
    Overflow,
    #[error("Haskell error called")]
    UserError,
    #[error("Haskell undefined forced")]
    Undefined,
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
/// The slot must remain valid and dereferenceable until `clear_rust_roots` is called.
pub unsafe fn register_rust_root(slot: *mut *mut u8) {
    RUST_ROOTS.with(|r| r.borrow_mut().push(slot));
}

/// Remove all registered Rust roots. Call after the GC-unsafe region ends.
pub fn clear_rust_roots() {
    RUST_ROOTS.with(|r| r.borrow_mut().clear());
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
pub fn clear_gc_state() {
    GC_STATE.with(|cell| {
        cell.borrow_mut().take();
    });
    clear_rust_roots();
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

    // External cancellation safepoint. We record `RuntimeError::Cancelled`
    // here but still perform a normal GC so the caller's allocation can
    // succeed cleanly — routing through `runtime_oom` is unsafe for Con
    // allocations larger than the 24-byte poison closure, which would be
    // written past. The cancellation is actually observed at the next
    // trampoline loop iteration (see `check_cancel_and_set_error` in the
    // trampolines), which returns the poison pointer in a context where it
    // will not be written to.
    if cancel_requested() {
        RUNTIME_ERROR.with(|cell| {
            let mut slot = cell.borrow_mut();
            if slot.is_none() {
                *slot = Some(RuntimeError::Cancelled);
            }
        });
        // Fall through to `perform_gc` so the allocation succeeds.
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
                    // allocated with the same size.
                    let result = unsafe {
                        tidepool_heap::gc::raw::cheney_copy(
                            &root_slots,
                            from_start as *const u8,
                            from_end as *const u8,
                            &mut tospace,
                        )
                    };

                    // Update GcState: swap to tospace
                    let to_start = tospace.as_mut_ptr();
                    state.active_start = to_start;
                    // active_size stays the same
                    state.active_buffer = Some(tospace); // drops old buffer if any

                    // SAFETY: vmctx is a valid pointer passed from JIT code. to_start points
                    // to the new tospace buffer which is now the active nursery.
                    unsafe {
                        (*vmctx).alloc_ptr = to_start.add(result.bytes_copied);
                        (*vmctx).alloc_limit = to_start.add(from_size) as *const u8;
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
                        let f: extern "C" fn(*mut VMContext, *mut u8) -> *mut u8 =
                            std::mem::transmute(code_ptr);
                        let result = f(vmctx, current);

                        // If GC ran during the call, current may have been forwarded.
                        // Check for forwarding pointer and follow it.
                        if heap_layout::read_tag(current) == layout::TAG_FORWARDED {
                            current = *(current.add(8) as *const *mut u8);
                        }

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
        *cell.borrow_mut() = Some(err);
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
        *cell.borrow_mut() = Some(err);
    });
    error_poison_ptr()
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

/// Trampoline for lazy poison closures. Reads the error kind from captured[0]
/// and calls `runtime_error(kind)` — setting the error flag only now, when the
/// closure is actually invoked.
// SAFETY: closure points to a lazy poison closure allocated by error_poison_ptr_lazy
// with captured[0] = error kind. arg may be null or a valid heap object.
unsafe extern "C" fn poison_trampoline_lazy(
    _vmctx: *mut VMContext,
    closure: *mut u8,
    arg: *mut u8,
) -> *mut u8 {
    let kind = *(closure.add(tidepool_heap::layout::CLOSURE_CAPTURED_OFFSET) as *const u64);

    // If the argument is a LitString, use it as the error message.
    if !arg.is_null() && tidepool_heap::layout::read_tag(arg) == tidepool_heap::layout::TAG_LIT {
        let lit_tag = *arg.add(tidepool_heap::layout::LIT_TAG_OFFSET);
        if lit_tag == 5 {
            // LIT_TAG_STRING
            let raw_ptr = *(arg.add(tidepool_heap::layout::LIT_VALUE_OFFSET) as *const *const u8);
            if !raw_ptr.is_null() {
                let len = *(raw_ptr as *const u64);
                let bytes_ptr = raw_ptr.add(8);
                return runtime_error_with_msg(kind, bytes_ptr, len);
            }
        }
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
pub fn take_runtime_error() -> Option<RuntimeError> {
    RUNTIME_ERROR.with(|cell| cell.borrow_mut().take())
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
            *cell.borrow_mut() = Some(RuntimeError::Undefined);
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
pub extern "C" fn runtime_new_byte_array(size: i64) -> i64 {
    if size < 0 {
        RUNTIME_ERROR.with(|cell| {
            *cell.borrow_mut() = Some(RuntimeError::UserErrorMsg(
                "negative size in byte array allocation".to_string(),
            ));
        });
        return error_poison_ptr() as i64;
    }
    let total = 8usize.saturating_add(size as usize);
    let layout =
        std::alloc::Layout::from_size_align(total, 8).unwrap_or_else(|_| std::process::abort());
    // SAFETY: alloc_zeroed returns a valid, zeroed allocation of the requested size.
    let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
    if ptr.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
    // SAFETY: ptr is a valid fresh allocation; writing the u64 length prefix at offset 0.
    unsafe {
        *(ptr as *mut u64) = size as u64;
    }
    ptr as i64
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
    // SAFETY: old_ptr passed the validity check above and has a u64 length prefix at offset 0.
    let old_size = unsafe { *(old_ptr as *const u64) } as usize;
    let new_size = new_size as usize;

    let new_total = 8usize.saturating_add(new_size);
    let new_layout =
        std::alloc::Layout::from_size_align(new_total, 8).unwrap_or_else(|_| std::process::abort());
    // SAFETY: alloc_zeroed returns a valid, zeroed allocation of the requested size.
    let new_ptr = unsafe { std::alloc::alloc_zeroed(new_layout) };
    if new_ptr.is_null() {
        std::alloc::handle_alloc_error(new_layout);
    }

    // Copy existing data (up to min of old/new size)
    let copy_len = old_size.min(new_size);
    // SAFETY: Both old and new buffers have data starting at offset 8.
    // copy_len <= min(old_size, new_size) so both reads and writes are in bounds.
    unsafe {
        std::ptr::copy_nonoverlapping(old_ptr.add(8), new_ptr.add(8), copy_len);
    }

    // SAFETY: new_ptr is a valid fresh allocation; writing the u64 length prefix.
    unsafe {
        *(new_ptr as *mut u64) = new_size as u64;
    }

    // Free old buffer
    let old_total = 8 + old_size;
    let old_layout =
        std::alloc::Layout::from_size_align(old_total, 8).unwrap_or_else(|_| std::process::abort());
    // SAFETY: old_ptr was allocated with this exact layout by a previous runtime_new/resize call.
    unsafe {
        std::alloc::dealloc(old_ptr, old_layout);
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
        let s = format!("{}", d);
        if s.contains('.') {
            s
        } else {
            format!("{}.0", s)
        }
    } else {
        // Scientific notation: Haskell uses e.g. "3.14e10"
        format!("{:e}", d)
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
pub extern "C" fn runtime_case_trap(scrut_ptr: i64, num_alts: i64, alt_tags: i64) -> *mut u8 {
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
        *cell.borrow_mut() = Some(RuntimeError::Undefined);
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
        let old_ptr = ptr as *mut u8;
        let size = *(old_ptr as *const u64) as usize;
        let layout = Layout::from_size_align(8 + size, 8).unwrap();
        dealloc(old_ptr, layout);
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

    #[test]
    fn test_runtime_shrink_byte_array() {
        unsafe {
            let ba = runtime_new_byte_array(10);
            runtime_shrink_byte_array(ba, 5);
            assert_eq!(*(ba as *const u64), 5);
            // Memory is still there, we just update the logical length prefix.
            // Note: we still need to free the original 10-byte allocation.
            let layout = Layout::from_size_align(8 + 10, 8).unwrap();
            dealloc(ba as *mut u8, layout);
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
        // Mirror of the read-side guard in heap_bridge.rs. If that constant
        // grows, POISON_BUF_SIZE must grow to match.
        const MAX_FIELDS: usize = 1024;
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
