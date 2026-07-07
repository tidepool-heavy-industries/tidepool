//! Per-machine ambient state, reached either via `VMContext.machine_state`
//! (callers holding a `vmctx`) or via the per-thread [`CURRENT_MACHINE`] slot
//! (vmctx-less host fns and the external ambient shims).
//!
//! Homes the state that used to live in per-thread `thread_local!` cells: the
//! external-cancellation flag, the JSON decode constructor ids, the stack-map
//! registry pointer, the call-depth counter (T6 leaf 1), the first-cause
//! runtime error, diagnostics, and parked-stream registry (T6 leaf 2), and the
//! GC state + GC root registries (T6 leaf 3: `GC_STATE`, `RUST_ROOTS`,
//! `PERSISTENT_ROOTS`). Each `JitEffectMachine` owns one `MachineState`
//! inline; `install_registries` points the run's `VMContext.machine_state` at
//! it and installs it as this thread's [`CURRENT_MACHINE`].
//!
//! ## GC-cluster reach (leaf 3): vmctx only, never `CURRENT_MACHINE`
//!
//! Unlike leaves 1/2, the GC cluster (`gc_state`/`rust_roots`/
//! `persistent_roots`) is reached EXCLUSIVELY via `VMContext.machine_state`
//! (through [`machine_state`] or a raw `(*vmctx).machine_state` check),
//! never through [`current_machine`]. A write (`register_rust_root`,
//! `register_persistent_root`) and the read that later traces it
//! (`perform_gc`) must key on the SAME machine or the collector can walk the
//! wrong heap; `CURRENT_MACHINE` is a per-THREAD slot that can point at a
//! different machine than the one a given `vmctx` belongs to (nested runs,
//! or a caller that captured a stale thread-local read), so it is not used
//! anywhere in the GC cluster. Every GC-root register/read site already has
//! (or is threaded to have) a `vmctx`; when `vmctx` is null OR
//! `(*vmctx).machine_state` is null, root registration/reads are a **no-op**
//! (register does nothing; reads return 0/empty/`None`) rather than a panic —
//! see the null-vmctx invariant on `RootScope`/`heap_to_value` in
//! `heap_bridge.rs` for why that no-op is temporally safe.
//!
//! GC-cluster **isolation invariant**: `MAX_CONCURRENT_EVALS` machines can be
//! live at once, one parked at `ask` on one thread, another running on
//! another. Per-machine (not process-global) GC state is what keeps two
//! concurrent evals' heaps from corrupting each other — a single global
//! `GC_STATE`/root-registry slot would let one eval's `perform_gc` walk (and
//! relocate objects in) a DIFFERENT eval's heap. This is why a process-global
//! GC pointer is forbidden here: every field lives on the `MachineState` the
//! running `vmctx` actually points at.
//!
//! `MachineState` is `pub`, and several of its methods plus
//! [`install_current_machine`]/[`restore_current_machine`] are `pub` (not
//! `pub(crate)`): a test that manually drives the JIT (without a
//! `JitEffectMachine`) owns one directly, wires `vmctx.machine_state` at it,
//! and/or installs it as `CURRENT_MACHINE`, exercising the same reach paths
//! as production instead of a test-only backdoor.

use std::cell::{Cell, RefCell, RefMut};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::context::VMContext;
use crate::host_fns::{GcState, ParkedStream, RuntimeError, StreamId};
use crate::stack_map::StackMapRegistry;

/// Per-machine ambient state. Every cell keeps the exact wrapper type
/// (`RefCell`/`Cell`) its thread-local predecessor used, preserving
/// try_borrow/borrow-panic/take semantics byte-for-byte.
pub struct MachineState {
    cancel_flag: RefCell<Option<Arc<AtomicBool>>>,
    json_con_ids: Cell<Option<tidepool_eval::json::JsonConIds>>,
    time_con_ids: Cell<Option<tidepool_eval::time::TimeConIds>>,
    stack_map_registry: RefCell<Option<*const StackMapRegistry>>,
    call_depth: Cell<u32>,
    runtime_error: RefCell<Option<RuntimeError>>,
    diagnostics: RefCell<Vec<String>>,
    parked_streams: RefCell<HashMap<StreamId, ParkedStream>>,
    stream_next_id: Cell<u64>,
    /// Bumped once per actual collection (`perform_gc`). `deep_force` (M3,
    /// repo-review-2026-07-06/01-gc-memory-safety.md) reads this to
    /// invalidate its address-keyed visited set whenever a GC could have
    /// relocated (or freed, then let something else reuse the address of)
    /// an object it recorded — an address-only check with no way to detect
    /// staleness would risk a false "already visited" hit after a
    /// collection reuses a since-vacated address for an unrelated object.
    gc_generation: Cell<u64>,
    gc_state: RefCell<Option<GcState>>,
    /// Run-scoped GC roots (mirrors the old `RUST_ROOTS` thread-local):
    /// heap-pointer slots registered by Rust host-fn frames the JIT frame
    /// walker cannot see. Cleared every `clear_run_scratch`/`clear_gc_state`.
    rust_roots: RefCell<Vec<*mut *mut u8>>,
    /// Session-scoped GC roots (mirrors the old `PERSISTENT_ROOTS`
    /// thread-local): tenured bindings' stable slots. Survive across runs;
    /// cleared only at machine teardown (`free_session_heap`).
    persistent_roots: RefCell<Vec<*mut *mut u8>>,
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
            time_con_ids: Cell::new(None),
            stack_map_registry: RefCell::new(None),
            call_depth: Cell::new(0),
            runtime_error: RefCell::new(None),
            diagnostics: RefCell::new(Vec::new()),
            parked_streams: RefCell::new(HashMap::new()),
            stream_next_id: Cell::new(1),
            gc_generation: Cell::new(0),
            gc_state: RefCell::new(None),
            rust_roots: RefCell::new(Vec::new()),
            persistent_roots: RefCell::new(Vec::new()),
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

    /// Pair with `incr_call_depth`: called when a non-tail call RETURNS, so
    /// the counter tracks the number of currently-active (unreturned) calls
    /// — actual nesting depth — instead of a monotonically increasing total.
    /// Saturating: never underflows past 0 even if some path double-decrements.
    pub(crate) fn decr_call_depth(&self) {
        self.call_depth.set(self.call_depth.get().saturating_sub(1));
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

    pub(crate) fn set_time_con_ids(&self, ids: Option<tidepool_eval::time::TimeConIds>) {
        self.time_con_ids.set(ids);
    }

    pub(crate) fn time_con_ids(&self) -> Option<tidepool_eval::time::TimeConIds> {
        self.time_con_ids.get()
    }

    // --- runtime error (first-cause cell) -----------------------------------

    /// Record `cause` unless an earlier cause is already recorded — first
    /// write wins, because the earliest record is the one closest to the
    /// fault.
    ///
    /// Uses `try_borrow_mut` defensively (M4, repo-review-2026-07-06/01-gc-
    /// memory-safety.md): `perform_gc` holds a RefMut across the Cheney copy,
    /// and a fault + `siglongjmp` there skips that RefMut's `Drop`, leaving
    /// the cell PERMANENTLY marked as mutably borrowed. A plain `borrow_mut`
    /// here would then panic — on the signal-recovery path, i.e. inside
    /// unwind/cleanup, which double-panics into `abort()` instead of
    /// surfacing `YieldError::Signal`. If the borrow fails we simply can't
    /// record this cause; silently dropping it (rather than panicking) is
    /// the same tradeoff `take_runtime_error` already makes.
    pub(crate) fn set_first_cause(&self, cause: RuntimeError) {
        if let Ok(mut slot) = self.runtime_error.try_borrow_mut() {
            if slot.is_none() {
                *slot = Some(cause);
            }
        }
    }

    /// Unconditionally overwrite the pending cause. Mirrors writers (e.g.
    /// `unresolved_var_trap`) that replace any earlier cause rather than
    /// preserving it.
    ///
    /// Same `try_borrow_mut` defense as [`Self::set_first_cause`] (M4) — this
    /// writer has the identical stuck-RefCell hazard as its sibling.
    pub(crate) fn set_runtime_error_overwrite(&self, cause: RuntimeError) {
        if let Ok(mut slot) = self.runtime_error.try_borrow_mut() {
            *slot = Some(cause);
        }
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

    /// Same `try_borrow` defense as [`Self::take_runtime_error`] (M4). Falls
    /// back to `true` (conservatively "yes, treat this as an error") rather
    /// than panicking — a caller asking this is about to gate on the answer,
    /// and if the cell is unreadable because something is mid-write on a
    /// signal-recovery path, the safe assumption is that there IS a pending
    /// cause, not that there isn't.
    pub(crate) fn has_runtime_error(&self) -> bool {
        self.runtime_error
            .try_borrow()
            .map(|e| e.is_some())
            .unwrap_or(true)
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

    // --- GC generation counter (M3) --------------------------------------

    /// Bump the generation counter. Called once per actual collection
    /// (`perform_gc`), never for a no-op `gc_trigger` that finds no work.
    pub(crate) fn bump_gc_generation(&self) {
        self.gc_generation
            .set(self.gc_generation.get().wrapping_add(1));
    }

    /// Current generation count, for detecting "did at least one collection
    /// run between these two points" (compare a snapshot taken before and
    /// after).
    pub(crate) fn gc_generation(&self) -> u64 {
        self.gc_generation.get()
    }

    // --- GC state (T6 leaf 3) --------------------------------------------
    // `set_gc_state`/`clear_gc_state` are `pub`: bare-VMContext test
    // harnesses (e.g. proptest_parked_registry.rs) own a MachineState, wire
    // `vmctx.machine_state` at it, and drive GC state directly — same
    // pattern leaf 1 used for `set_stack_map_registry`. The rest are
    // `pub(crate)`: their only callers (`install_registries`,
    // `RegistryGuard::drop`, `JitEffectMachine::drop`, `make_session_vmctx`)
    // are all in this crate and hold `self.machine_state`/a guard pointer.

    /// Set the active GC region for this machine. Mirrors the old
    /// `GC_STATE.with(|cell| *cell.borrow_mut() = Some(GcState { .. }))` body.
    ///
    /// Uses `try_borrow_mut` defensively (M4): `perform_gc` holds a `gc_state`
    /// `RefMut` across the Cheney copy, and a fault + `siglongjmp` there skips
    /// that `RefMut`'s `Drop`, permanently marking the cell mutably borrowed.
    /// A plain `borrow_mut` here would then panic on the signal-recovery
    /// path — same defense as `take_runtime_error`. If the borrow fails we
    /// simply can't install the new region; the caller surfaces
    /// `YieldError::Signal` instead of this panicking.
    pub fn set_gc_state(&self, start: *mut u8, size: usize) {
        if let Ok(mut slot) = self.gc_state.try_borrow_mut() {
            *slot = Some(GcState {
                active_start: start,
                active_size: size,
                active_buffer: None,
            });
        }
    }

    /// Install a retained session heap buffer as the active GC region (see
    /// the free-fn doc this replaces, `host_fns::gc::install_session_buffer`).
    ///
    /// Same `try_borrow_mut` defense as [`Self::set_gc_state`] (M4).
    pub(crate) fn install_session_buffer(&self, mut buffer: Vec<u64>) {
        let start = buffer.as_mut_ptr() as *mut u8;
        let size = buffer.len() * 8;
        if let Ok(mut slot) = self.gc_state.try_borrow_mut() {
            *slot = Some(GcState {
                active_start: start,
                active_size: size,
                active_buffer: Some(buffer),
            });
        }
    }

    /// Reclaim the live heap buffer + high-water cursor from this machine's
    /// GC state, called from `RegistryGuard::drop` BEFORE `clear_run_scratch`
    /// takes the `GcState`. See the free-fn doc this replaces for the
    /// `(buffer, cursor)` contract.
    ///
    /// Uses `try_borrow_mut` defensively (M4): this runs from `Drop`, exactly
    /// where a stuck-mutably-borrowed `gc_state` cell (left behind by a fault
    /// and `siglongjmp` during `perform_gc`'s Cheney copy) is most dangerous
    /// — a plain `borrow_mut` panicking here panics INSIDE a `Drop`, which
    /// during unwind aborts the whole process instead of surfacing
    /// `YieldError::Signal`. Falls back to `(None, 0)`, the same shape
    /// already used when there's no `GcState` at all.
    pub(crate) fn reclaim_session_heap(&self, alloc_ptr: *mut u8) -> (Option<Vec<u64>>, usize) {
        match self.gc_state.try_borrow_mut() {
            Ok(mut guard) => match guard.as_mut() {
                Some(state) => {
                    let cursor = (alloc_ptr as usize).saturating_sub(state.active_start as usize);
                    let buf = state.active_buffer.take();
                    (buf, cursor)
                }
                None => (None, 0),
            },
            Err(_) => (None, 0),
        }
    }

    /// The current active GC region as `(start, size_bytes)`, or `None` if no
    /// GC state is installed on this machine.
    pub(crate) fn gc_active_range(&self) -> Option<(*mut u8, usize)> {
        self.gc_state
            .borrow()
            .as_ref()
            .map(|s| (s.active_start, s.active_size))
    }

    /// Clear this machine's GC state and run-scoped rust roots. One-shot
    /// teardown path (mirrors the old `clear_gc_state` free fn).
    pub fn clear_gc_state(&self) {
        self.gc_state.borrow_mut().take();
        self.clear_rust_roots();
    }

    /// PER-RUN teardown: take `GcState` (the `active_buffer` was already
    /// reclaimed by `reclaim_session_heap` before this runs) and clear the
    /// per-run rust roots. Does NOT touch `persistent_roots` — those are
    /// session-scoped and survive until `free_session_heap`.
    pub(crate) fn clear_run_scratch(&self) {
        self.gc_state.borrow_mut().take();
        self.clear_rust_roots();
    }

    /// MACHINE-DROP teardown: clear session-scoped persistent roots and take
    /// `GcState`. Called by `JitEffectMachine::drop`. Operates directly on
    /// `self` (not through any ambient reach) so it always clears exactly
    /// the dying machine's own registries.
    pub(crate) fn free_session_heap(&self) {
        self.clear_persistent_roots();
        self.gc_state.borrow_mut().take();
    }

    /// Borrow this machine's `GcState` cell mutably — used by `perform_gc`'s
    /// Cheney-copy body, which needs to swap `active_buffer` in place.
    pub(crate) fn gc_state_mut(&self) -> RefMut<'_, Option<GcState>> {
        self.gc_state.borrow_mut()
    }

    // --- rust roots (run-scoped GC roots, T6 leaf 3) ----------------------

    pub(crate) fn register_rust_root(&self, slot: *mut *mut u8) {
        self.rust_roots.borrow_mut().push(slot);
    }

    pub(crate) fn rust_roots_len(&self) -> usize {
        self.rust_roots.borrow().len()
    }

    pub(crate) fn truncate_rust_roots(&self, mark: usize) {
        self.rust_roots.borrow_mut().truncate(mark);
    }

    pub(crate) fn clear_rust_roots(&self) {
        self.rust_roots.borrow_mut().clear();
    }

    /// Append this machine's run-scoped rust roots to `out` — used by
    /// `perform_gc` to build its root slot list (byte-identical logic to the
    /// old `RUST_ROOTS.with(|r| root_slots.extend(r.borrow().iter().copied()))`).
    pub(crate) fn extend_rust_roots(&self, out: &mut Vec<*mut *mut u8>) {
        out.extend(self.rust_roots.borrow().iter().copied());
    }

    // --- persistent roots (session-scoped GC roots, T6 leaf 3) ------------

    pub(crate) fn register_persistent_root(&self, slot: *mut *mut u8) {
        self.persistent_roots.borrow_mut().push(slot);
    }

    /// Number of registered persistent roots (test/diagnostic accessor).
    pub(crate) fn persistent_roots_count(&self) -> usize {
        self.persistent_roots.borrow().len()
    }

    pub(crate) fn clear_persistent_roots(&self) {
        self.persistent_roots.borrow_mut().clear();
    }

    /// Append this machine's session-scoped persistent roots to `out` — the
    /// persistent-root sibling of `extend_rust_roots`, used by `perform_gc`.
    pub(crate) fn extend_persistent_roots(&self, out: &mut Vec<*mut *mut u8>) {
        out.extend(self.persistent_roots.borrow().iter().copied());
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

/// Null-safe sibling of [`machine_state`] for the GC-cluster register/read
/// sites (leaf 3): `register_rust_root`/`rust_roots_mark`/
/// `truncate_rust_roots`/`clear_rust_roots`/`register_persistent_root`/
/// `persistent_roots_count` all reach through this instead of panicking on a
/// null `vmctx`. Returns `None` — a legitimate no-op, not an error — when
/// `vmctx` is null (the `heap_to_value` null-vmctx bridge path; see the
/// invariant on `RootScope` in `heap_bridge.rs` for why that is temporally
/// safe) OR `(*vmctx).machine_state` is null (a hand-built `VMContext` in a
/// unit test that never wired a machine, e.g. `force.rs`'s raw-thunk tests).
///
/// # Safety
/// If `vmctx` is non-null, it must point to a live `VMContext`.
pub(crate) unsafe fn machine_state_opt<'a>(vmctx: *mut VMContext) -> Option<&'a MachineState> {
    if vmctx.is_null() || (*vmctx).machine_state.is_null() {
        None
    } else {
        Some(&*(*vmctx).machine_state)
    }
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
    /// Elimination path: leaf 3 threads `vmctx` into every GC-cluster
    /// register/read site instead of routing them through this cell (see the
    /// module-level "GC-cluster reach" note — a per-thread slot can diverge
    /// from the `vmctx` a write/read actually belongs to, which the GC
    /// cluster cannot tolerate), shrinking this cell's remaining internal
    /// callers to the two external shims (`set_first_cause`/
    /// `drain_diagnostics`, anchored to #340), which keep using it until that
    /// sibling-crate cutover captures a machine handle at suspension time
    /// instead. Full host-fn vmctx-reach (`runtime_error`/
    /// `runtime_error_with_msg`/`unresolved_var_trap`/`runtime_shape_trap`/
    /// `runtime_oom`/the array primops) is #329.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// M4 (repo-review-2026-07-06/01-gc-memory-safety.md, Medium findings):
    /// a fault + `siglongjmp` during `perform_gc`'s Cheney copy skips the
    /// `RefMut` guard's `Drop`, leaving `gc_state`/`runtime_error`
    /// PERMANENTLY marked as mutably borrowed (`RefCell` has no "unpoison"
    /// once a guard's release never runs). We reproduce that exact
    /// `RefCell` state directly — hold a live `borrow_mut()` guard across
    /// the calls under test — rather than actually raising a signal;
    /// `signal_safety.rs` separately covers signal delivery/recovery
    /// itself. Before the fix, every one of these panicked (a plain
    /// `borrow`/`borrow_mut` on an already-mutably-borrowed cell); after,
    /// each returns its documented graceful fallback instead.
    #[test]
    fn stuck_runtime_error_cell_does_not_panic() {
        let ms = MachineState::new();
        let _guard = ms.runtime_error.borrow_mut(); // simulates a stuck signal-path borrow

        // has_runtime_error: conservative `true` fallback, not a panic.
        assert!(ms.has_runtime_error());
        // set_first_cause / set_runtime_error_overwrite: silently no-op, not a panic.
        ms.set_first_cause(RuntimeError::Cancelled);
        ms.set_runtime_error_overwrite(RuntimeError::Cancelled);
        // take_runtime_error (already fixed pre-M4): None, not a panic.
        assert_eq!(ms.take_runtime_error(), None);
    }

    #[test]
    fn stuck_gc_state_cell_does_not_panic() {
        let ms = MachineState::new();
        ms.set_gc_state(std::ptr::null_mut(), 0);
        let _guard = ms.gc_state.borrow_mut(); // simulates a stuck signal-path borrow

        // set_gc_state / install_session_buffer: silently no-op, not a panic.
        ms.set_gc_state(std::ptr::dangling_mut(), 128);
        ms.install_session_buffer(vec![0u64; 1]);
        // reclaim_session_heap: (None, 0) fallback, not a panic (this is the
        // one the plan calls out as running from `RegistryGuard::drop` —
        // panicking here is a panic-inside-Drop, which during unwind
        // aborts the process instead of surfacing `YieldError::Signal`).
        assert_eq!(ms.reclaim_session_heap(std::ptr::null_mut()), (None, 0));
    }
}
