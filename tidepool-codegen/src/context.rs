use std::mem;

/// VM context passed as implicit first argument to all JIT-compiled functions.
///
/// Layout is frozen for offsets 0-32: gc_trigger reads fields by offset.
/// alloc_ptr at 0, alloc_limit at 8, gc_trigger at 16, tail_callee at 24, tail_arg at 32.
/// `machine_state` at 40 is host-fn-only ambient state (see
/// `crate::machine_state`) and MUST NEVER be loaded by JIT-emitted code —
/// only passed through as the `vmctx` argument to a host-fn call.
/// `prepared_tops` at 48 and `prepared_stack_limit` at 56 are connected
/// prepared-program state. Generated prepared code may load the latter when
/// performing its native stack preflight; legacy code must leave it unused.
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
    /// Per-machine ambient state (cancellation, JSON con ids, stack-map
    /// registry, call depth, diagnostics, ...). Null until installed by
    /// `JitEffectMachine::install_registries` (or wired directly onto a
    /// manually-constructed VMContext by a test).
    pub machine_state: *mut crate::machine_state::MachineState,
    /// Invocation-owned compact top-binding table for connected prepared code.
    /// Entries are immutable static managed values or pinned raw byte addresses.
    pub prepared_tops: *const usize,
    /// Lowest permitted stack pointer for prepared native code. This is the
    /// native stack low bound plus the finalized-frame reserve; null disables
    /// the prepared stack preflight for legacy effect-machine entries.
    pub prepared_stack_limit: *const u8,
}

impl VMContext {
    /// Create a new VMContext with the given nursery region and GC trigger.
    /// `machine_state` starts null; callers install it separately.
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
            machine_state: std::ptr::null_mut(),
            prepared_tops: std::ptr::null(),
            prepared_stack_limit: std::ptr::null(),
        }
    }
}

// Compile-time offset assertions
const _: () = {
    use crate::layout::*;
    assert!(mem::offset_of!(VMContext, alloc_ptr) == VMCTX_ALLOC_PTR_OFFSET as usize);
    assert!(mem::offset_of!(VMContext, alloc_limit) == VMCTX_ALLOC_LIMIT_OFFSET as usize);
    assert!(mem::offset_of!(VMContext, gc_trigger) == VMCTX_GC_TRIGGER_OFFSET as usize);
    assert!(mem::offset_of!(VMContext, tail_callee) == VMCTX_TAIL_CALLEE_OFFSET as usize);
    assert!(mem::offset_of!(VMContext, tail_arg) == VMCTX_TAIL_ARG_OFFSET as usize);
    assert!(
        mem::offset_of!(VMContext, prepared_stack_limit)
            == VMCTX_PREPARED_STACK_LIMIT_OFFSET as usize
    );
    assert!(mem::align_of::<VMContext>() == 16);
};
