use std::mem;

/// VM context passed as implicit first argument to all JIT-compiled functions.
///
/// `machine_state` is host-fn-only ambient state (see
/// `crate::machine_state`) and MUST NEVER be loaded by JIT-emitted code —
/// only passed through as the `vmctx` argument to a host-fn call.
/// `prepared_stack_limit` is connected prepared-program state:
/// generated prepared code may load it when performing its native stack
/// preflight; legacy code must leave it unused. Prepared code reaches its
/// tops through `root_tables`: the machine's table of installed images'
/// root blocks, indexed by the image slot the code embeds, so one compiled
/// image runs on any machine that installed it.
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
    /// This machine's root-block table: one block pointer per installed
    /// image slot (`crate::prepared_program::ImageSlot`), null for a slot
    /// the machine has not installed. Generated code loads the table base
    /// here, then its own image's block, then the top word. Only a machine
    /// at a quiescent point replaces the table (installs grow it).
    pub root_tables: *const *mut u64,
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
            root_tables: std::ptr::null(),
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
    assert!(mem::offset_of!(VMContext, root_tables) == VMCTX_ROOT_TABLES_OFFSET as usize);
    assert!(mem::align_of::<VMContext>() == 16);
};
