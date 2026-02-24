use crate::context::VMContext;
use crate::gc::frame_walker::{self, StackRoot};
use crate::stack_map::StackMapRegistry;
use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

type GcHook = fn(&[StackRoot]);

/// Runtime errors raised by JIT code via host functions.
#[derive(Debug, Clone)]
pub enum RuntimeError {
    DivisionByZero,
    Overflow,
    UserError,
    Undefined,
    TypeMetadata,
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuntimeError::DivisionByZero => write!(f, "division by zero"),
            RuntimeError::Overflow => write!(f, "arithmetic overflow"),
            RuntimeError::UserError => write!(f, "Haskell error called"),
            RuntimeError::Undefined => write!(f, "Haskell undefined forced"),
            RuntimeError::TypeMetadata => write!(f, "forced type metadata (should be dead code)"),
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
}

/// GC trigger: called by JIT code when alloc_ptr exceeds alloc_limit.
///
/// This function MUST be compiled with frame pointers preserved
/// (the whole crate uses preserve_frame_pointers, and the Rust profile
/// should have force-frame-pointers = true for the gc path).
///
/// The frame walker in gc_trigger reads RBP to walk the JIT stack.
#[inline(never)]
pub extern "C" fn gc_trigger(vmctx: *mut VMContext) {
    // Force a frame to be created
    let mut _dummy = [0u64; 2];
    std::hint::black_box(&mut _dummy);

    GC_TRIGGER_CALL_COUNT.fetch_add(1, Ordering::SeqCst);
    GC_TRIGGER_LAST_VMCTX.store(vmctx as usize, Ordering::SeqCst);

    #[cfg(target_arch = "x86_64")]
    {
        let rbp: usize;
        let rsp: usize;
        unsafe {
            std::arch::asm!("mov {}, rbp", out(reg) rbp, options(nomem, nostack));
            std::arch::asm!("mov {}, rsp", out(reg) rsp, options(nomem, nostack));
        }

        STACK_MAP_REGISTRY.with(|reg_cell| {
            if let Some(registry_ptr) = *reg_cell.borrow() {
                let registry = unsafe { &*registry_ptr };
                // Walk frames starting from gc_trigger's own frame.
                let roots = unsafe { frame_walker::walk_frames(rbp, registry, rsp) };
                
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
pub extern "C" fn heap_force(_vmctx: *mut VMContext, _thunk: *mut u8) -> *mut u8 {
    std::ptr::null_mut() // Placeholder for scaffold
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
    eprintln!(
        "[FATAL] Forced unresolved external variable: VarId({:#x}) [tag='{}', key={}]",
        var_id, tag_char, key
    );
    eprintln!("  Backtrace:");
    let bt = std::backtrace::Backtrace::force_capture();
    eprintln!("{}", bt);
    std::process::abort();
}

/// Called by JIT code for runtime errors (divZeroError, overflowError).
/// Sets a thread-local error flag and returns null. The effect machine
/// checks this after JIT returns and converts to Yield::Error.
/// kind: 0 = divZeroError, 1 = overflowError
pub extern "C" fn runtime_error(kind: u64) -> *mut u8 {
    let err_name = match kind {
        0 => "DivisionByZero",
        1 => "Overflow",
        2 => "UserError",
        3 => "Undefined",
        4 => "TypeMetadata",
        _ => "Unknown",
    };
    eprintln!("[JIT] runtime_error called: kind={} ({})", kind, err_name);
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
    std::ptr::null_mut()
}

/// Check and take any pending runtime error from JIT code.
pub fn take_runtime_error() -> Option<RuntimeError> {
    RUNTIME_ERROR.with(|cell| cell.borrow_mut().take())
}

/// Return the list of host function symbols for JIT registration.
///
/// Usage: `CodegenPipeline::new(&host_fn_symbols())`
/// Debug: called before every App call_indirect to validate the function pointer.
/// Prints the heap tag and code_ptr. Aborts on non-closure.
pub extern "C" fn debug_app_check(fun_ptr: *const u8) {
    if fun_ptr.is_null() {
        eprintln!("[JIT] App: fun_ptr is NULL");
        std::process::abort();
    }
    let tag = unsafe { *fun_ptr };
    if tag != tidepool_heap::layout::TAG_CLOSURE {
        let tag_name = match tag {
            0 => "Closure",
            1 => "Thunk",
            2 => "Con",
            3 => "Lit",
            _ => "UNKNOWN",
        };
        eprintln!(
            "[JIT] App: fun_ptr={:p} has tag {} ({}) — expected Closure!",
            fun_ptr, tag, tag_name
        );
        // Read more context for debugging
        if tag == tidepool_heap::layout::TAG_CON {
            let con_tag = unsafe { *(fun_ptr.add(8) as *const u64) };
            let num_fields = unsafe { *(fun_ptr.add(16) as *const u16) };
            eprintln!("[JIT]   Con tag={}, num_fields={}", con_tag, num_fields);
        }
        std::process::abort();
    }
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
    unsafe { *(ptr as *mut u64) = size as u64; }
    ptr as i64
}

/// Copy `len` bytes from `src` (Addr#) to `dest_ba` (ByteArray ptr) at `dest_off`.
pub extern "C" fn runtime_copy_addr_to_byte_array(src: i64, dest_ba: i64, dest_off: i64, len: i64) {
    let src_ptr = src as *const u8;
    let dest_ptr = unsafe { (dest_ba as *mut u8).add(8 + dest_off as usize) };
    unsafe { std::ptr::copy_nonoverlapping(src_ptr, dest_ptr, len as usize); }
}

/// Set `len` bytes in `ba` starting at `off` to `val`.
pub extern "C" fn runtime_set_byte_array(ba: i64, off: i64, len: i64, val: i64) {
    let ptr = unsafe { (ba as *mut u8).add(8 + off as usize) };
    unsafe { std::ptr::write_bytes(ptr, val as u8, len as usize); }
}

/// Shrink a mutable byte array to `new_size` bytes (just updates the length prefix).
pub extern "C" fn runtime_shrink_byte_array(ba: i64, new_size: i64) {
    unsafe { *(ba as *mut u64) = new_size as u64; }
}

pub fn host_fn_symbols() -> Vec<(&'static str, *const u8)> {
    vec![
        ("gc_trigger", gc_trigger as *const u8),
        ("heap_alloc", heap_alloc as *const u8),
        ("heap_force", heap_force as *const u8),
        ("unresolved_var_trap", unresolved_var_trap as *const u8),
        ("runtime_error", runtime_error as *const u8),
        ("debug_app_check", debug_app_check as *const u8),
        ("runtime_new_byte_array", runtime_new_byte_array as *const u8),
        ("runtime_copy_addr_to_byte_array", runtime_copy_addr_to_byte_array as *const u8),
        ("runtime_set_byte_array", runtime_set_byte_array as *const u8),
        ("runtime_shrink_byte_array", runtime_shrink_byte_array as *const u8),
    ]
}
