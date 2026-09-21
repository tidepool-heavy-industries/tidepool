use std::mem;

/// VM context passed as implicit first argument to all JIT-compiled functions.
///
/// `machine_state` is host-fn-only ambient state (see
/// `crate::machine_state`) and MUST NEVER be loaded by JIT-emitted code —
/// only passed through as the `vmctx` argument to a host-fn call.
/// `prepared_stack_limit` is connected prepared-program state:
/// generated prepared code may load it when performing its native stack
/// preflight; legacy code must leave it unused. Prepared code reaches its
/// tops through its own program's root block, whose address it embeds, not
/// through this context.
#[repr(C, align(16))]
pub struct VMContext {
    /// Current bump-pointer allocation cursor.
    pub alloc_ptr: *mut u8,
    /// End of the current nursery region.
    pub alloc_limit: *const u8,
    /// Per-machine ambient state (cancellation, JSON con ids, stack-map
    /// registry, call depth, diagnostics, ...). Null until installed by
    /// `PreparedMachine::install_registries` (or wired directly onto a
    /// manually-constructed VMContext by a test).
    pub machine_state: *mut crate::machine_state::MachineState,
    /// Lowest permitted stack pointer for prepared native code. This is the
    /// native stack low bound plus the finalized-frame reserve; null disables
    /// the prepared stack preflight for legacy effect-machine entries.
    pub prepared_stack_limit: *const u8,
}

impl VMContext {
    /// Create a new VMContext with the given nursery region.
    /// `machine_state` starts null; callers install it separately.
    pub fn new(nursery_start: *mut u8, nursery_end: *const u8) -> Self {
        Self {
            alloc_ptr: nursery_start,
            alloc_limit: nursery_end,
            machine_state: std::ptr::null_mut(),
            prepared_stack_limit: std::ptr::null(),
        }
    }
}

// Compile-time offset assertions
const _: () = {
    use crate::layout::*;
    assert!(mem::offset_of!(VMContext, alloc_ptr) == VMCTX_ALLOC_PTR_OFFSET as usize);
    assert!(mem::offset_of!(VMContext, alloc_limit) == VMCTX_ALLOC_LIMIT_OFFSET as usize);
    assert!(
        mem::offset_of!(VMContext, prepared_stack_limit)
            == VMCTX_PREPARED_STACK_LIMIT_OFFSET as usize
    );
    assert!(mem::align_of::<VMContext>() == 16);
};
