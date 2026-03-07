use std::mem;

/// VM context passed as implicit first argument to all JIT-compiled functions.
///
/// Layout is frozen: gc_trigger reads fields by offset.
/// alloc_ptr at 0, alloc_limit at 8, gc_trigger at 16, tail_callee at 24, tail_arg at 32.
#[repr(C, align(16))]
pub struct VMContext {
    /// Current bump-pointer allocation cursor.
    pub alloc_ptr: *mut u8,
    /// End of the current nursery region.
    pub alloc_limit: *const u8,
    /// Host function called when alloc_ptr exceeds alloc_limit.
    pub gc_trigger: unsafe extern "C" fn(*mut VMContext),
    /// TCO: pending tail-call callee (closure pointer), null if no pending tail call.
    pub tail_callee: *mut u8,
    /// TCO: pending tail-call argument, null if no pending tail call.
    pub tail_arg: *mut u8,
}

impl VMContext {
    /// Create a new VMContext with the given nursery region and GC trigger.
    pub fn new(
        nursery_start: *mut u8,
        nursery_end: *const u8,
        gc_trigger: unsafe extern "C" fn(*mut VMContext),
    ) -> Self {
        Self {
            alloc_ptr: nursery_start,
            alloc_limit: nursery_end,
            gc_trigger,
            tail_callee: std::ptr::null_mut(),
            tail_arg: std::ptr::null_mut(),
        }
    }
}

// Compile-time offset assertions
const _: () = {
    assert!(mem::offset_of!(VMContext, alloc_ptr) == 0);
    assert!(mem::offset_of!(VMContext, alloc_limit) == 8);
    assert!(mem::offset_of!(VMContext, gc_trigger) == 16);
    assert!(mem::offset_of!(VMContext, tail_callee) == 24);
    assert!(mem::offset_of!(VMContext, tail_arg) == 32);
    assert!(mem::align_of::<VMContext>() == 16);
};
