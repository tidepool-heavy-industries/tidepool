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
    use std::io::Write;
    let _ = std::io::stderr().flush();
    std::process::abort();
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
    // Return a poison object instead of null. This is a valid Lit(Int#, 0)
    // heap object, so JIT code won't segfault when reading its tag byte.
    // The effect machine will detect the error flag and return Yield::Error
    // before this poison value reaches user code.
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

/// Check and take any pending runtime error from JIT code.
pub fn take_runtime_error() -> Option<RuntimeError> {
    RUNTIME_ERROR.with(|cell| cell.borrow_mut().take())
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
pub unsafe extern "C" fn debug_app_check(fun_ptr: *const u8) {
    use std::io::Write;
    // If a runtime error is already pending, don't abort on tag mismatches —
    // we're in error-propagation mode and the effect machine will handle it.
    let has_error = RUNTIME_ERROR.with(|cell| cell.borrow().is_some());
    if fun_ptr.is_null() {
        if has_error {
            return; // Error already flagged, just continue
        }
        eprintln!("[JIT] App: fun_ptr is NULL — unresolved binding");
        eprintln!("[JIT] Backtrace:\n{:?}", std::backtrace::Backtrace::force_capture());
        let _ = std::io::stderr().flush();
        std::process::abort();
    }
    let tag = unsafe { *fun_ptr };
    if tag != tidepool_heap::layout::TAG_CLOSURE {
        if has_error {
            return; // Error already flagged, tag mismatch is expected (poison object)
        }
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
        if tag == tidepool_heap::layout::TAG_CON {
            let con_tag = unsafe { *(fun_ptr.add(8) as *const u64) };
            let num_fields = unsafe { *(fun_ptr.add(16) as *const u16) };
            eprintln!("[JIT]   Con tag={}, num_fields={}", con_tag, num_fields);
        }
        let _ = std::io::stderr().flush();
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
    unsafe { *(new_ptr as *mut u64) = new_size as u64; }

    // Free old buffer
    let old_total = 8 + old_size;
    let old_layout = std::alloc::Layout::from_size_align(old_total, 8).unwrap();
    unsafe { std::alloc::dealloc(old_ptr, old_layout); }

    new_ptr as i64
}

/// Copy `len` bytes between byte arrays: src[src_off..] -> dest[dest_off..].
/// Used by both copyByteArray# and copyMutableByteArray#.
pub extern "C" fn runtime_copy_byte_array(src: i64, src_off: i64, dest: i64, dest_off: i64, len: i64) {
    let src_ptr = unsafe { (src as *const u8).add(8 + src_off as usize) };
    let dest_ptr = unsafe { (dest as *mut u8).add(8 + dest_off as usize) };
    // Use copy (not copy_nonoverlapping) since src and dest may be the same array
    unsafe { std::ptr::copy(src_ptr, dest_ptr, len as usize); }
}

/// Compare byte arrays: returns -1, 0, or 1.
pub extern "C" fn runtime_compare_byte_arrays(a: i64, a_off: i64, b: i64, b_off: i64, len: i64) -> i64 {
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

/// strlen: count bytes until null terminator.
pub extern "C" fn runtime_strlen(addr: i64) -> i64 {
    let ptr = addr as *const u8;
    let mut len = 0i64;
    unsafe {
        while *ptr.add(len as usize) != 0 {
            len += 1;
        }
    }
    len
}

/// Measure the number of bytes in `len` UTF-8 codepoints starting at `addr + off`.
pub extern "C" fn runtime_text_measure_off(addr: i64, off: i64, len: i64) -> i64 {
    let ptr = (addr + off) as *const u8;
    let mut byte_count = 0i64;
    let mut chars_left = len;
    while chars_left > 0 {
        let b = unsafe { *ptr.add(byte_count as usize) };
        let char_len = if b < 0x80 { 1 }
            else if b < 0xE0 { 2 }
            else if b < 0xF0 { 3 }
            else { 4 };
        byte_count += char_len;
        chars_left -= 1;
    }
    byte_count
}

/// Find a byte in a buffer. Returns offset from start, or -1 if not found.
pub extern "C" fn runtime_text_memchr(addr: i64, off: i64, len: i64, needle: i64) -> i64 {
    let ptr = (addr + off) as *const u8;
    let slice = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    match slice.iter().position(|&b| b == needle as u8) {
        Some(pos) => pos as i64,
        None => -1,
    }
}

/// Reverse UTF-8 text: reverse codepoints from src into dest.
pub extern "C" fn runtime_text_reverse(dest: i64, len: i64, src: i64) {
    let src_slice = unsafe { std::slice::from_raw_parts(src as *const u8, len as usize) };
    let dest_ptr = dest as *mut u8;
    // Decode UTF-8 codepoints, write in reverse order
    let mut read_pos = 0usize;
    let mut write_pos = len as usize;
    while read_pos < len as usize {
        let b = src_slice[read_pos];
        let char_len = if b < 0x80 { 1 }
            else if b < 0xE0 { 2 }
            else if b < 0xF0 { 3 }
            else { 4 };
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
        ("runtime_resize_byte_array", runtime_resize_byte_array as *const u8),
        ("runtime_copy_byte_array", runtime_copy_byte_array as *const u8),
        ("runtime_compare_byte_arrays", runtime_compare_byte_arrays as *const u8),
        ("runtime_strlen", runtime_strlen as *const u8),
        ("runtime_text_measure_off", runtime_text_measure_off as *const u8),
        ("runtime_text_memchr", runtime_text_memchr as *const u8),
        ("runtime_text_reverse", runtime_text_reverse as *const u8),
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

    #[test]
    fn test_runtime_text_measure_off_ascii() {
        let s = b"hello";
        assert_eq!(runtime_text_measure_off(s.as_ptr() as i64, 0, 5), 5);
        assert_eq!(runtime_text_measure_off(s.as_ptr() as i64, 1, 3), 3);
    }

    #[test]
    fn test_runtime_text_measure_off_multi_byte() {
        // "λ" is 2 bytes: CF BB
        // "😀" is 4 bytes: F0 9F 98 80
        let s = "λ😀x".as_bytes();
        assert_eq!(runtime_text_measure_off(s.as_ptr() as i64, 0, 1), 2); // λ
        assert_eq!(runtime_text_measure_off(s.as_ptr() as i64, 0, 2), 6); // λ😀
        assert_eq!(runtime_text_measure_off(s.as_ptr() as i64, 2, 1), 4); // 😀
        assert_eq!(runtime_text_measure_off(s.as_ptr() as i64, 6, 1), 1); // x
    }

    #[test]
    fn test_runtime_text_memchr() {
        let s = b"abacaba";
        assert_eq!(runtime_text_memchr(s.as_ptr() as i64, 0, 7, b'a' as i64), 0);
        assert_eq!(runtime_text_memchr(s.as_ptr() as i64, 1, 6, b'a' as i64), 1); // 'a' at index 2 of original, which is offset 1 from s+1
        assert_eq!(runtime_text_memchr(s.as_ptr() as i64, 0, 7, b'z' as i64), -1);
    }

    #[test]
    fn test_runtime_text_reverse_ascii() {
        let src = b"hello";
        let mut dest = [0u8; 5];
        runtime_text_reverse(dest.as_mut_ptr() as i64, 5, src.as_ptr() as i64);
        assert_eq!(&dest, b"olleh");
    }

    #[test]
    fn test_runtime_text_reverse_utf8() {
        // "λ😀" -> CF BB | F0 9F 98 80
        // Reversed should be "😀λ" -> F0 9F 98 80 | CF BB
        let src = "λ😀".as_bytes();
        let mut dest = [0u8; 6];
        runtime_text_reverse(dest.as_mut_ptr() as i64, 6, src.as_ptr() as i64);
        assert_eq!(std::str::from_utf8(&dest).unwrap(), "😀λ");
    }

    #[test]
    fn test_runtime_text_measure_complex() {
        // "Aλ文😀" 
        // A: 1 byte
        // λ: 2 bytes
        // 文: 3 bytes (E6 96 87)
        // 😀: 4 bytes
        let s = "Aλ文😀".as_bytes();
        assert_eq!(runtime_text_measure_off(s.as_ptr() as i64, 0, 4), 1 + 2 + 3 + 4);
        assert_eq!(runtime_text_measure_off(s.as_ptr() as i64, 1, 2), 2 + 3);
    }
}
