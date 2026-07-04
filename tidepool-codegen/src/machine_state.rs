//! Per-machine ambient state, reached either via `VMContext.machine_state`
//! (callers holding a `vmctx`) or via the per-thread [`CURRENT_MACHINE`] slot
//! (vmctx-less host fns and the external ambient shims).
//!
//! Homes the state that used to live in per-thread `thread_local!` cells: the
//! external-cancellation flag, the JSON decode constructor ids, the stack-map
//! registry pointer, the call-depth counter (T6 leaf 1), and the first-cause
//! runtime error, diagnostics, and parked-stream registry (T6 leaf 2). Each
//! `JitEffectMachine` owns one `MachineState` inline; `install_registries`
//! points the run's `VMContext.machine_state` at it and installs it as this
//! thread's [`CURRENT_MACHINE`]. Leaf 3 adds the GC-state fields (GC_STATE,
//! RUST_ROOTS, PERSISTENT_ROOTS).
//!
//! `MachineState` is `pub`, and several of its methods plus
//! [`install_current_machine`]/[`restore_current_machine`] are `pub` (not
//! `pub(crate)`): a test that manually drives the JIT (without a
//! `JitEffectMachine`) owns one directly, wires `vmctx.machine_state` at it,
//! and/or installs it as `CURRENT_MACHINE`, exercising the same reach paths
//! as production instead of a test-only backdoor.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::context::VMContext;
use crate::host_fns::{ParkedStream, RuntimeError, StreamId};
use crate::stack_map::StackMapRegistry;

/// Per-machine ambient state. Every cell keeps the exact wrapper type
/// (`RefCell`/`Cell`) its thread-local predecessor used, preserving
/// try_borrow/borrow-panic/take semantics byte-for-byte.
pub struct MachineState {
    cancel_flag: RefCell<Option<Arc<AtomicBool>>>,
    json_con_ids: Cell<Option<tidepool_eval::json::JsonConIds>>,
    stack_map_registry: RefCell<Option<*const StackMapRegistry>>,
    call_depth: Cell<u32>,
    runtime_error: RefCell<Option<RuntimeError>>,
    diagnostics: RefCell<Vec<String>>,
    parked_streams: RefCell<HashMap<StreamId, ParkedStream>>,
    stream_next_id: Cell<u64>,
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
            runtime_error: RefCell::new(None),
            diagnostics: RefCell::new(Vec::new()),
            parked_streams: RefCell::new(HashMap::new()),
            stream_next_id: Cell::new(1),
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

    // --- runtime error (first-cause cell) -----------------------------------

    /// Record `cause` unless an earlier cause is already recorded — first
    /// write wins, because the earliest record is the one closest to the
    /// fault.
    pub(crate) fn set_first_cause(&self, cause: RuntimeError) {
        let mut slot = self.runtime_error.borrow_mut();
        if slot.is_none() {
            *slot = Some(cause);
        }
    }

    /// Unconditionally overwrite the pending cause. Mirrors writers (e.g.
    /// `unresolved_var_trap`) that replace any earlier cause rather than
    /// preserving it.
    pub(crate) fn set_runtime_error_overwrite(&self, cause: RuntimeError) {
        *self.runtime_error.borrow_mut() = Some(cause);
    }

    /// Take the pending cause, if any. Uses `try_borrow_mut` defensively: this
    /// runs on the signal/teardown path, and a signal can fire while JIT host
    /// code still holds a `borrow_mut` on the cell — a plain `borrow_mut`
    /// would then panic (and panicking inside `Drop`/unwind double-panics →
    /// `abort()`).
    pub(crate) fn take_runtime_error(&self) -> Option<RuntimeError> {
        self.runtime_error
            .try_borrow_mut()
            .ok()
            .and_then(|mut e| e.take())
    }

    pub(crate) fn has_runtime_error(&self) -> bool {
        self.runtime_error.borrow().is_some()
    }

    // --- diagnostics ---------------------------------------------------------

    pub(crate) fn push_diagnostic(&self, msg: String) {
        self.diagnostics.borrow_mut().push(msg);
    }

    pub(crate) fn drain_diagnostics(&self) -> Vec<String> {
        self.diagnostics.borrow_mut().drain(..).collect()
    }

    // --- parked streams --------------------------------------------------------

    /// Park a response stream; returns the registry id carried by tail thunks.
    pub(crate) fn park_stream(&self, stream: ParkedStream) -> u64 {
        let id = self.stream_next_id.get();
        self.stream_next_id.set(id + 1);
        self.parked_streams
            .borrow_mut()
            .insert(StreamId(id), stream);
        id
    }

    /// Drop all parked streams (machine teardown).
    pub(crate) fn clear_parked_streams(&self) {
        self.parked_streams.borrow_mut().clear();
    }

    /// Read-only access to a parked stream by id (mirrors `PARKED_STREAMS
    /// .with(|r| r.borrow().get(...))`).
    pub(crate) fn parked_stream_get<R>(
        &self,
        id: StreamId,
        f: impl FnOnce(&ParkedStream) -> R,
    ) -> Option<R> {
        self.parked_streams.borrow().get(&id).map(f)
    }

    /// Mutable access to a parked stream by id (mirrors `PARKED_STREAMS
    /// .with(|r| r.borrow_mut().get_mut(...))`).
    pub(crate) fn parked_stream_get_mut<R>(
        &self,
        id: StreamId,
        f: impl FnOnce(&mut ParkedStream) -> R,
    ) -> Option<R> {
        self.parked_streams.borrow_mut().get_mut(&id).map(f)
    }

    /// Remove a parked stream by id (source exhausted).
    pub(crate) fn remove_parked_stream(&self, id: StreamId) {
        self.parked_streams.borrow_mut().remove(&id);
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

thread_local! {
    /// Per-thread reach for vmctx-less callers: host fns (called from
    /// JIT/emitted code) that take no `vmctx`, and the external ambient
    /// shims (`set_first_cause`/`take_runtime_error`/`has_runtime_error`/
    /// `push_diagnostic`/`drain_diagnostics`/`park_stream`/
    /// `clear_parked_streams`). Each eval runs on its own dedicated thread
    /// (server.rs spawns one per eval, up to `MAX_CONCURRENT_EVALS`
    /// concurrently, and a suspended eval keeps its thread + machine +
    /// `RegistryGuard` alive across the suspension) — so per-thread reach is
    /// per-eval reach, matching the correctness the per-thread thread-locals
    /// this replaces already had. A process-global slot would be a
    /// cancellation regression here: two machines CAN be live at once, and a
    /// global pointer would let one eval's abort land on another's machine.
    ///
    /// State itself still lives on [`MachineState`], owned per-machine; this
    /// cell is only the reach path for code that has no `vmctx` to follow.
    ///
    /// Elimination path: leaf 3 threads `vmctx` into the allocating GC_STATE
    /// writers, shrinking this cell's remaining internal callers to the two
    /// external shims (`set_first_cause`/`drain_diagnostics`, anchored to
    /// #340), which keep using it until that sibling-crate cutover captures a
    /// machine handle at suspension time instead. Full host-fn vmctx-reach
    /// (`runtime_error`/`runtime_error_with_msg`/`unresolved_var_trap`/
    /// `runtime_case_trap`/`runtime_oom`/the array primops) is #329.
    static CURRENT_MACHINE: Cell<*mut MachineState> = const { Cell::new(std::ptr::null_mut()) };
}

/// Install `ms` as this thread's current machine, returning the
/// previously-installed pointer (null if none). Called by
/// `JitEffectMachine::install_registries`; the returned previous value is
/// restored by `RegistryGuard::drop`. `pub` (not `pub(crate)`): bare-VMContext
/// test harnesses in `tests/` (separate crates, no `JitEffectMachine`) call
/// this directly on their own `MachineState`, exercising the same reach path
/// as production instead of a test-only backdoor — same rationale as the
/// `pub` `MachineState` methods above.
pub fn install_current_machine(ms: *mut MachineState) -> *mut MachineState {
    CURRENT_MACHINE.with(|c| c.replace(ms))
}

/// Restore a previously-saved current-machine pointer (see
/// [`install_current_machine`]). Called by `RegistryGuard::drop`; `pub` for
/// the same bare-VMContext test-harness reason as `install_current_machine`.
pub fn restore_current_machine(prev: *mut MachineState) {
    CURRENT_MACHINE.with(|c| c.set(prev));
}

/// Reach this thread's current machine, if any. Used by the ambient free-fn
/// shims and by host fns without a `vmctx`.
///
/// # Safety
/// The returned reference must not be retained past the call that produced
/// it: the pointee is owned by a `JitEffectMachine` whose run may end (and
/// clear `CURRENT_MACHINE`) at any safepoint.
pub(crate) unsafe fn current_machine<'a>() -> Option<&'a MachineState> {
    let p = CURRENT_MACHINE.with(|c| c.get());
    if p.is_null() {
        None
    } else {
        Some(&*p)
    }
}

/// Test-only support for exercising the ambient shims / vmctx-less host fns
/// outside a full `JitEffectMachine` run.
#[cfg(test)]
pub(crate) mod test_support {
    use super::{install_current_machine, restore_current_machine, MachineState};

    /// Install a fresh throwaway `MachineState` as this thread's current
    /// machine for the duration of `f`, restoring whatever was previously
    /// installed afterward (mirrors `install_registries`/`RegistryGuard::drop`
    /// without needing a full `JitEffectMachine`).
    pub(crate) fn with_test_machine<R>(f: impl FnOnce() -> R) -> R {
        struct Restore(*mut MachineState);
        impl Drop for Restore {
            fn drop(&mut self) {
                restore_current_machine(self.0);
            }
        }
        let ms = MachineState::new();
        // Declared after `ms`, so this drops before `ms` on every exit —
        // return OR unwind — restoring CURRENT_MACHINE off `ms` before `ms`
        // is freed, so a panicking `f()` cannot leave a dangling pointer for
        // a later test on the same worker thread.
        let _restore = Restore(install_current_machine(
            &ms as *const MachineState as *mut MachineState,
        ));
        f()
    }
}
