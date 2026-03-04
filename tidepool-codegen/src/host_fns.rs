use crate::context::VMContext;
use crate::gc::frame_walker::{self, StackRoot};
use crate::stack_map::StackMapRegistry;
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use tidepool_heap::layout;

type GcHook = fn(&[StackRoot]);

/// Runtime errors raised by JIT code via host functions.
#[derive(Debug, Clone)]
pub enum RuntimeError {
    DivisionByZero,
    Overflow,
    UserError,
    Undefined,
    TypeMetadata,
    UnresolvedVar(u64),
    NullFunPtr,
    BadFunPtrTag(u8),
    HeapOverflow,
    StackOverflow,
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuntimeError::DivisionByZero => write!(f, "division by zero"),
            RuntimeError::Overflow => write!(f, "arithmetic overflow"),
            RuntimeError::UserError => write!(f, "Haskell error called"),
            RuntimeError::Undefined => write!(f, "Haskell undefined forced"),
            RuntimeError::TypeMetadata => write!(f, "forced type metadata (should be dead code)"),
            RuntimeError::UnresolvedVar(id) => {
                let tag_char = (*id >> 56) as u8 as char;
                let key = *id & ((1u64 << 56) - 1);
                write!(
                    f,
                    "unresolved variable VarId({:#x}) [tag='{}', key={}]",
                    id, tag_char, key
                )
            }
            RuntimeError::NullFunPtr => write!(f, "application of null function pointer"),
            RuntimeError::BadFunPtrTag(tag) => {
                write!(f, "application of non-closure (tag={})", tag)
            }
            RuntimeError::HeapOverflow => write!(f, "heap overflow (nursery exhausted after GC)"),
            RuntimeError::StackOverflow => write!(f, "stack overflow (likely infinite list or unbounded recursion — use zipWithIndex/imap/enumFromTo instead of [0..])"),
        }
    }
}

thread_local! {
    /// Registry of stack maps for JIT functions.
    /// This is set before calling into JIT code so gc_trigger can access it.
    static STACK_MAP_REGISTRY: RefCell<Option<*const StackMapRegistry>> = const { RefCell::new(None) };

    /// Collected roots from the last gc_trigger call.
    /// Used for test inspection.
    static LAST_ROOTS: RefCell<Vec<StackRoot>> = const { RefCell::new(Vec::new()) };

    static HOOK: RefCell<Option<GcHook>> = const { RefCell::new(None) };

    /// Runtime error from JIT code. Checked after JIT returns.
    static RUNTIME_ERROR: RefCell<Option<RuntimeError>> = const { RefCell::new(None) };

    pub(crate) static GC_STATE: RefCell<Option<GcState>> = const { RefCell::new(None) };

    /// Call depth counter for detecting runaway recursion (e.g. infinite lists).
    /// Reset before each JIT invocation; incremented in debug_app_check.
    static CALL_DEPTH: Cell<u32> = const { Cell::new(0) };

    /// Captured JIT diagnostics.
    static DIAGNOSTICS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
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

    #[cfg(target_arch = "x86_64")]
    {
        let fp: usize;
        unsafe {
            std::arch::asm!("mov {}, rbp", out(reg) fp, options(nomem, nostack));
        }
        perform_gc(fp, vmctx);
    }

    #[cfg(target_arch = "aarch64")]
    {
        let fp: usize;
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
            let registry = unsafe { &*registry_ptr };
            // Walk frames starting from gc_trigger's own frame.
            let roots = unsafe { frame_walker::walk_frames(fp, registry) };

            // ── Cheney copying GC ──────────────────────────────
            GC_STATE.with(|gc_cell| {
                let mut gc_state = gc_cell.borrow_mut();
                if let Some(state) = gc_state.as_mut() {
                    let from_start = state.active_start;
                    let from_size = state.active_size;
                    let from_end = unsafe { from_start.add(from_size) };

                    let mut tospace = vec![0u8; from_size];

                    // Convert StackRoot to raw slot pointers
                    let root_slots: Vec<*mut *mut u8> = roots
                        .iter()
                        .map(|r| r.stack_slot_addr as *mut *mut u8)
                        .collect();

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

                    // Update VMContext for resumed allocation
                    unsafe {
                        (*vmctx).alloc_ptr = to_start.add(result.bytes_copied);
                        (*vmctx).alloc_limit = to_start.add(from_size) as *const u8;
                    }
                }
            });
            // ── End GC ─────────────────────────────────────────

            // Call test hook if present
            HOOK.with(|hook_cell| {
                if let Some(hook) = *hook_cell.borrow() {
                    hook(&roots);
                }
            });

            LAST_ROOTS.with(|roots_cell| {
                *roots_cell.borrow_mut() = roots;
            });
        }
    });
}

/// Set a hook to be called during gc_trigger with the collected roots.
pub fn set_gc_test_hook(hook: GcHook) {
    HOOK.with(|hook_cell| {
        *hook_cell.borrow_mut() = Some(hook);
    });
}

/// Clear the GC test hook.
pub fn clear_gc_test_hook() {
    HOOK.with(|hook_cell| {
        *hook_cell.borrow_mut() = None;
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

/// Get collected roots from the last gc_trigger call.
pub fn last_gc_roots() -> Vec<StackRoot> {
    LAST_ROOTS.with(|roots_cell| roots_cell.borrow().clone())
}

/// Heap allocation: called by JIT code for large or slow-path allocations.
pub extern "C" fn heap_alloc(_vmctx: *mut VMContext, _size: u64) -> *mut u8 {
    std::ptr::null_mut() // Placeholder for scaffold
}

/// Force a thunk to WHNF.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn heap_force(vmctx: *mut VMContext, obj: *mut u8) -> *mut u8 {
    if obj.is_null() {
        return obj;
    }

    unsafe {
        let tag = layout::read_tag(obj);
        if tag >= 2 {
            return obj; // Con or Lit - already WHNF
        }
        if tag != layout::TAG_CLOSURE {
            return obj; // Thunk (tag=1) or unknown - not handled here
        }

        // Closure: read code_ptr
        let code_ptr_val = *(obj.add(layout::CLOSURE_CODE_PTR_OFFSET) as *const usize);

        if code_ptr_val == 0 {
            return obj;
        }

        // Force the closure. In a data-case scrutinee position, GHC Core
        // guarantees the result must be a data constructor, so any closure
        // here is a thunk (suspended computation) regardless of capture count.
        // SAFETY: code_ptr is a JIT-compiled function pointer. The JIT guarantees
        // it points to a function with this exact signature (closure calling convention).
        let f: extern "C" fn(*mut VMContext, *mut u8, *mut u8) -> *mut u8 =
            std::mem::transmute(code_ptr_val);
        f(vmctx, obj, std::ptr::null_mut())
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
    LAST_ROOTS.with(|roots_cell| {
        roots_cell.borrow_mut().clear();
    });
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

pub extern "C" fn runtime_oom() -> *mut u8 {
    RUNTIME_ERROR.with(|cell| {
        *cell.borrow_mut() = Some(RuntimeError::HeapOverflow);
    });
    error_poison_ptr()
}

/// Return a pointer to a pre-allocated "poison" Closure heap object.
/// When JIT code tries to call this as a function, it returns itself,
/// preventing cascading crashes. The runtime error flag is already set,
/// so the effect machine will catch it before the poison reaches user code.
pub fn error_poison_ptr() -> *mut u8 {
    use std::sync::OnceLock;
    // Layout: Closure with code_ptr pointing to `poison_trampoline`,
    // num_captured = 0. When called, returns the poison closure itself.
    static POISON: OnceLock<usize> = OnceLock::new();
    let addr = *POISON.get_or_init(|| {
        // Closure size: header(8) + code_ptr(8) + num_captured(8) = 24
        let size = 24usize;
        let layout = std::alloc::Layout::from_size_align(size, 8).unwrap();
        let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
        if ptr.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        unsafe {
            tidepool_heap::layout::write_header(
                ptr,
                tidepool_heap::layout::TAG_CLOSURE,
                size as u16,
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
            let lo = std::alloc::Layout::from_size_align(size, 8).unwrap();
            let ptr = unsafe { std::alloc::alloc_zeroed(lo) };
            if ptr.is_null() {
                std::alloc::handle_alloc_error(lo);
            }
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
unsafe extern "C" fn poison_trampoline_lazy(
    _vmctx: *mut VMContext,
    closure: *mut u8,
    _arg: *mut u8,
) -> *mut u8 {
    let kind = *(closure.add(tidepool_heap::layout::CLOSURE_CAPTURED_OFFSET) as *const u64);
    runtime_error(kind)
}

/// Check and take any pending runtime error from JIT code.
pub fn take_runtime_error() -> Option<RuntimeError> {
    RUNTIME_ERROR.with(|cell| cell.borrow_mut().take())
}

/// Reset the call depth counter. Call before each JIT invocation.
pub fn reset_call_depth() {
    CALL_DEPTH.with(|c| c.set(0));
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
const MAX_CALL_DEPTH: u32 = 50_000;

/// Returns 0 if the call is safe to proceed, or a poison pointer if the call
/// should be short-circuited (runtime error already set or call depth exceeded).
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
            let con_tag = unsafe { *(fun_ptr.add(8) as *const u64) };
            let num_fields = unsafe { *(fun_ptr.add(16) as *const u16) };
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
    let total = 8 + size as usize;
    let layout = std::alloc::Layout::from_size_align(total, 8).unwrap();
    let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
    if ptr.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
    // Write length prefix
    unsafe {
        *(ptr as *mut u64) = size as u64;
    }
    ptr as i64
}

/// Copy `len` bytes from `src` (Addr#) to `dest_ba` (ByteArray ptr) at `dest_off`.
pub extern "C" fn runtime_copy_addr_to_byte_array(src: i64, dest_ba: i64, dest_off: i64, len: i64) {
    if (src as u64) < 0x1000 || (dest_ba as u64) < 0x1000 {
        eprintln!("[BUG] runtime_copy_addr_to_byte_array: bad pointer src={:#x} dest_ba={:#x} dest_off={} len={}", src, dest_ba, dest_off, len);
        std::process::abort();
    }
    let dest_size = unsafe { *(dest_ba as *const u64) } as usize;
    if (dest_off as usize + len as usize) > dest_size {
        eprintln!(
            "[BUG] runtime_copy_addr_to_byte_array: out of bounds! size={} off={} len={}",
            dest_size, dest_off, len
        );
        std::process::abort();
    }
    let src_ptr = src as *const u8;
    let dest_ptr = unsafe { (dest_ba as *mut u8).add(8 + dest_off as usize) };
    unsafe {
        std::ptr::copy_nonoverlapping(src_ptr, dest_ptr, len as usize);
    }
}

/// Set `len` bytes in `ba` starting at `off` to `val`.
pub extern "C" fn runtime_set_byte_array(ba: i64, off: i64, len: i64, val: i64) {
    if (ba as u64) < 0x1000 {
        eprintln!(
            "[BUG] runtime_set_byte_array: bad pointer ba={:#x} off={} len={} val={}",
            ba, off, len, val
        );
        std::process::abort();
    }
    let ptr = unsafe { (ba as *mut u8).add(8 + off as usize) };
    unsafe {
        std::ptr::write_bytes(ptr, val as u8, len as usize);
    }
}

/// Shrink a mutable byte array to `new_size` bytes (just updates the length prefix).
pub extern "C" fn runtime_shrink_byte_array(ba: i64, new_size: i64) {
    unsafe {
        *(ba as *mut u64) = new_size as u64;
    }
}

/// Resize a mutable byte array. Allocates a new buffer, copies existing data,
/// zeroes any new bytes, and frees the old buffer. Returns the new pointer.
pub extern "C" fn runtime_resize_byte_array(ba: i64, new_size: i64) -> i64 {
    let old_ptr = ba as *mut u8;
    let old_size = unsafe { *(old_ptr as *const u64) } as usize;
    let new_size = new_size as usize;

    let new_total = 8 + new_size;
    let new_layout = std::alloc::Layout::from_size_align(new_total, 8).unwrap();
    let new_ptr = unsafe { std::alloc::alloc_zeroed(new_layout) };
    if new_ptr.is_null() {
        std::alloc::handle_alloc_error(new_layout);
    }

    // Copy existing data (up to min of old/new size)
    let copy_len = old_size.min(new_size);
    unsafe {
        std::ptr::copy_nonoverlapping(old_ptr.add(8), new_ptr.add(8), copy_len);
    }

    // Write new length prefix
    unsafe {
        *(new_ptr as *mut u64) = new_size as u64;
    }

    // Free old buffer
    let old_total = 8 + old_size;
    let old_layout = std::alloc::Layout::from_size_align(old_total, 8).unwrap();
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
    if (src as u64) < 0x1000 || (dest as u64) < 0x1000 {
        eprintln!("[BUG] runtime_copy_byte_array: bad pointer src={:#x} src_off={} dest={:#x} dest_off={} len={}", src, src_off, dest, dest_off, len);
        std::process::abort();
    }
    let src_ptr = unsafe { (src as *const u8).add(8 + src_off as usize) };
    let dest_ptr = unsafe { (dest as *mut u8).add(8 + dest_off as usize) };
    // Use copy (not copy_nonoverlapping) since src and dest may be the same array
    unsafe {
        std::ptr::copy(src_ptr, dest_ptr, len as usize);
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
    if (a as u64) < 0x1000 || (b as u64) < 0x1000 {
        eprintln!("[BUG] runtime_compare_byte_arrays: bad pointer a={:#x} a_off={} b={:#x} b_off={} len={}", a, a_off, b, b_off, len);
        std::process::abort();
    }
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
/// Layout: [u64 length][ptr0][ptr1]...[ptrN-1]
/// Each slot is 8 bytes (a heap pointer).
pub extern "C" fn runtime_new_boxed_array(len: i64, init: i64) -> i64 {
    let n = len as usize;
    let total = 8 + 8 * n;
    let layout = std::alloc::Layout::from_size_align(total, 8).unwrap();
    let ptr = unsafe { std::alloc::alloc(layout) };
    if ptr.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
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
    let n = len as usize;
    let total = 8 + 8 * n;
    let layout = std::alloc::Layout::from_size_align(total, 8).unwrap();
    let ptr = unsafe { std::alloc::alloc(layout) };
    if ptr.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
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
    let src_ptr = unsafe { (src as *const u8).add(8 + 8 * src_off as usize) };
    let dest_ptr = unsafe { (dest as *mut u8).add(8 + 8 * dest_off as usize) };
    unsafe {
        std::ptr::copy(src_ptr, dest_ptr, 8 * len as usize);
    }
}

/// Shrink a boxed array (just update the length field).
pub extern "C" fn runtime_shrink_boxed_array(arr: i64, new_len: i64) {
    unsafe {
        *(arr as *mut u64) = new_len as u64;
    }
}

/// CAS on a boxed array slot: compare-and-swap arr[idx].
/// Returns the old value. If old == expected, writes new.
pub extern "C" fn runtime_cas_boxed_array(arr: i64, idx: i64, expected: i64, new: i64) -> i64 {
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
    if (addr as u64) < 0x1000 {
        eprintln!("[BUG] runtime_strlen: bad pointer addr={:#x}", addr);
        std::process::abort();
    }
    let ptr = addr as *const u8;
    let mut len = 0i64;
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
    if (addr as u64) < 0x1000 {
        eprintln!(
            "[BUG] runtime_text_measure_off: bad pointer addr={:#x} off={} len={} cnt={}",
            addr, off, len, cnt
        );
        std::process::abort();
    }
    let ptr = (addr + off) as *const u8;
    let len = len as usize;
    let cnt = cnt as usize;
    let mut byte_pos = 0usize;
    let mut chars_found = 0usize;
    while chars_found < cnt && byte_pos < len {
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
    if (addr as u64) < 0x1000 {
        eprintln!(
            "[BUG] runtime_text_memchr: bad pointer addr={:#x} off={} len={} needle={}",
            addr, off, len, needle
        );
        std::process::abort();
    }
    let ptr = (addr + off) as *const u8;
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
    if (dest as u64) < 0x1000 || (src as u64) < 0x1000 {
        eprintln!(
            "[BUG] runtime_text_reverse: bad pointer dest={:#x} src={:#x} off={} len={}",
            dest, src, off, len
        );
        std::process::abort();
    }
    let src_ptr = (src + off) as *const u8;
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
    let c_str = std::ffi::CString::new(s).unwrap();
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
        ("heap_alloc", heap_alloc as *const u8),
        ("heap_force", heap_force as *const u8),
        ("unresolved_var_trap", unresolved_var_trap as *const u8),
        ("runtime_error", runtime_error as *const u8),
        ("debug_app_check", debug_app_check as *const u8),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::alloc::{dealloc, Layout};

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
}

/// Debug: called instead of `trap user2` when TIDEPOOL_DEBUG_CASE is set.
/// Prints diagnostic info about the scrutinee that failed case matching.
/// `scrut_ptr` is the heap pointer to the scrutinee.
/// `num_alts` is the number of data alt tags expected.
/// `alt_tags` is a pointer to an array of expected tag u64 values.
pub extern "C" fn runtime_case_trap(scrut_ptr: i64, num_alts: i64, alt_tags: i64) -> *mut u8 {
    use std::io::Write;
    let ptr = scrut_ptr as *const u8;
    if (scrut_ptr as u64) < 0x1000 {
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(
            stderr,
            "[CASE TRAP] scrut_ptr is NULL/invalid: {:#x}",
            scrut_ptr
        );
        let _ = stderr.flush();
        std::process::abort();
    }
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
    let expected: Vec<u64> = if num_alts > 0 && alt_tags != 0 {
        (0..num_alts as usize)
            .map(|i| unsafe { *((alt_tags as *const u64).add(i)) })
            .collect()
    } else {
        vec![]
    };

    // Dump raw bytes for any object type
    let raw_bytes: Vec<u8> = (0..32).map(|i| unsafe { *ptr.add(i) }).collect();
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "[CASE TRAP] raw bytes: {:02x?}", raw_bytes);

    if tag_byte == 2 {
        let con_tag = unsafe { *(ptr.add(8) as *const u64) };
        let num_fields = unsafe { *(ptr.add(16) as *const u16) };
        let _ = writeln!(
            stderr,
            "[CASE TRAP] Con: con_tag={:#x}, num_fields={}, expected_tags={:?}",
            con_tag, num_fields, expected
        );
    } else if tag_byte == 3 {
        let lit_tag = unsafe { *(ptr.add(8) as *const u64) };
        let value = unsafe { *(ptr.add(16) as *const u64) };
        let _ = writeln!(
            stderr,
            "[CASE TRAP] Lit: lit_tag={:#x}, value={:#x}, expected_tags={:?}",
            lit_tag, value, expected
        );
    } else if tag_byte == 0 {
        let code_ptr = unsafe { *(ptr.add(8) as *const u64) };
        let num_captured = unsafe { *(ptr.add(16) as *const u16) };
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
    std::process::abort();
}
