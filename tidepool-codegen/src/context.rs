use std::mem;

/// VM context passed as implicit first argument to all JIT-compiled functions.
///
/// Layout is frozen: gc_trigger reads fields by offset.
/// alloc_ptr at 0, alloc_limit at 8, gc_trigger at 16.
#[repr(C, align(16))]
pub struct VMContext {
    /// Current bump-pointer allocation cursor.
    pub alloc_ptr: *mut u8,
    /// End of the current nursery region.
    pub alloc_limit: *const u8,
    /// Host function called when alloc_ptr exceeds alloc_limit.
    pub gc_trigger: unsafe extern "C" fn(*mut VMContext),
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
        }
    }
}

// Compile-time offset assertions
const _: () = {
    assert!(mem::offset_of!(VMContext, alloc_ptr) == 0);
    assert!(mem::offset_of!(VMContext, alloc_limit) == 8);
    assert!(mem::offset_of!(VMContext, gc_trigger) == 16);
    assert!(mem::align_of::<VMContext>() == 16);
};
