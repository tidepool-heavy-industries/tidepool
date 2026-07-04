//! Per-machine ambient state, reached via `VMContext.machine_state`.
//!
//! Homes the state that used to live in per-thread `thread_local!` cells:
//! the external-cancellation flag, the JSON decode constructor ids, the
//! stack-map registry pointer, and the call-depth counter (T6 leaf 1). Each
//! `JitEffectMachine` owns one `MachineState` inline; `install_registries`
//! points the run's `VMContext.machine_state` at it. DIAGNOSTICS stays
//! thread-local for now — it's consumed zero-arg by sibling crates outside
//! this migration's edit boundary, so its cutover is deferred to leaf 2
//! alongside RUNTIME_ERROR (same sibling-crate coordination). Leaves 2 and 3
//! add more fields (RUNTIME_ERROR, DIAGNOSTICS, PARKED_STREAMS,
//! STREAM_NEXT_ID, GC_STATE, RUST_ROOTS, PERSISTENT_ROOTS) to this struct.
//!
//! `MachineState` is `pub`: a test that manually drives the JIT (without a
//! `JitEffectMachine`) owns one directly and wires `vmctx.machine_state` at
//! it, exercising the same reach path as production instead of a test-only
//! backdoor.

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::context::VMContext;
use crate::stack_map::StackMapRegistry;

/// Per-machine ambient state. Every cell keeps the exact wrapper type
/// (`RefCell`/`Cell`) its thread-local predecessor used, preserving
/// try_borrow/borrow-panic/take semantics byte-for-byte.
pub struct MachineState {
    cancel_flag: RefCell<Option<Arc<AtomicBool>>>,
    json_con_ids: Cell<Option<tidepool_eval::json::JsonConIds>>,
    stack_map_registry: RefCell<Option<*const StackMapRegistry>>,
    call_depth: Cell<u32>,
}

// SAFETY: MachineState is only ever accessed from the single thread driving
// the owning JitEffectMachine's run; the raw stack-map pointer is never
// dereferenced off that thread.
unsafe impl Send for MachineState {}

impl MachineState {
    pub fn new() -> Self {
        Self {
            cancel_flag: RefCell::new(None),
            json_con_ids: Cell::new(None),
            stack_map_registry: RefCell::new(None),
            call_depth: Cell::new(0),
        }
    }

    // --- stack map registry ---------------------------------------------
    // `pub`: bare-VMContext test harnesses (separate crates, no
    // JitEffectMachine) install this directly on their own MachineState.

    pub fn set_stack_map_registry(&self, registry: &StackMapRegistry) {
        *self.stack_map_registry.borrow_mut() = Some(registry as *const _);
    }

    pub fn clear_stack_map_registry(&self) {
        *self.stack_map_registry.borrow_mut() = None;
    }

    pub(crate) fn stack_map_registry(&self) -> Option<*const StackMapRegistry> {
        *self.stack_map_registry.borrow()
    }

    // --- call depth ------------------------------------------------------
    // `pub`: bare-VMContext test harnesses reset this directly.

    pub fn reset_call_depth(&self) {
        self.call_depth.set(0);
    }

    pub(crate) fn incr_call_depth(&self) -> u32 {
        let d = self.call_depth.get() + 1;
        self.call_depth.set(d);
        d
    }

    // --- cancel flag -------------------------------------------------------

    pub(crate) fn set_cancel_flag(&self, flag: Arc<AtomicBool>) {
        *self.cancel_flag.borrow_mut() = Some(flag);
    }

    pub(crate) fn clear_cancel_flag(&self) {
        self.cancel_flag.borrow_mut().take();
    }

    pub(crate) fn cancel_requested(&self) -> bool {
        self.cancel_flag
            .borrow()
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
    }

    // --- JSON decode constructor ids ---------------------------------------

    pub(crate) fn set_json_con_ids(&self, ids: Option<tidepool_eval::json::JsonConIds>) {
        self.json_con_ids.set(ids);
    }

    pub(crate) fn json_con_ids(&self) -> Option<tidepool_eval::json::JsonConIds> {
        self.json_con_ids.get()
    }
}

impl Default for MachineState {
    fn default() -> Self {
        Self::new()
    }
}

/// Reach the per-machine ambient state from a live `VMContext`. Host fns
/// that hold `vmctx` use this. Never consulted by the `heap_to_value`
/// null-vmctx path — leaf-1 fields are not reached from there.
///
/// # Safety
/// `vmctx` must be non-null and `(*vmctx).machine_state` must have been
/// installed (by `JitEffectMachine::install_registries`, or wired directly
/// onto a manually-constructed `VMContext` in a test) before this is called.
pub(crate) unsafe fn machine_state<'a>(vmctx: *mut VMContext) -> &'a MachineState {
    debug_assert!(!vmctx.is_null() && !(*vmctx).machine_state.is_null());
    &*(*vmctx).machine_state
}
