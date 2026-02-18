use crate::context::VMContext;
use crate::gc::frame_walker::{self, StackRoot};
use crate::stack_map::StackMapRegistry;
use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

type GcHook = fn(&[StackRoot]);

thread_local! {
    /// Registry of stack maps for JIT functions.
    /// This is set before calling into JIT code so gc_trigger can access it.
    static STACK_MAP_REGISTRY: RefCell<Option<*const StackMapRegistry>> = const { RefCell::new(None) };

    /// Collected roots from the last gc_trigger call.
    /// Used for test inspection.
    static LAST_ROOTS: RefCell<Vec<StackRoot>> = const { RefCell::new(Vec::new()) };

    static HOOK: RefCell<Option<GcHook>> = const { RefCell::new(None) };
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
