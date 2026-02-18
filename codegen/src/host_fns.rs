use crate::context::VMContext;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// GC trigger: called by JIT code when alloc_ptr exceeds alloc_limit.
///
/// This function MUST be compiled with frame pointers preserved
/// (the whole crate uses preserve_frame_pointers, and the Rust profile
/// should have force-frame-pointers = true for the gc path).
///
/// The frame walker in gc_trigger reads RBP to walk the JIT stack.
pub extern "C" fn gc_trigger(vmctx: *mut VMContext) {
    // Placeholder: in the full implementation, this will:
    // 1. Walk the JIT stack via RBP chain
    // 2. Collect GC roots from stack maps
    // 3. Run the copying collector
    // 4. Update forwarding pointers in stack slots
    // 5. Update vmctx alloc_ptr/alloc_limit to new nursery
    //
    // For scaffold tests, just record that it was called.
    GC_TRIGGER_CALL_COUNT.fetch_add(1, Ordering::SeqCst);
    GC_TRIGGER_LAST_VMCTX.store(vmctx as usize, Ordering::SeqCst);
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
}

/// Get gc_trigger call count. Only call from tests.
pub fn gc_trigger_call_count() -> u64 {
    GC_TRIGGER_CALL_COUNT.load(Ordering::SeqCst)
}

/// Get last vmctx passed to gc_trigger. Only call from tests.
pub fn gc_trigger_last_vmctx() -> usize {
    GC_TRIGGER_LAST_VMCTX.load(Ordering::SeqCst)
}

/// Return the list of host function symbols for JIT registration.
///
/// Usage: `CodegenPipeline::new(&host_fn_symbols())`
pub fn host_fn_symbols() -> Vec<(&'static str, *const u8)> {
    vec![
        ("gc_trigger", gc_trigger as *const u8),
        ("heap_alloc", heap_alloc as *const u8),
        ("heap_force", heap_force as *const u8),
    ]
}
