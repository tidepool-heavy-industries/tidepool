//! Per-machine ambient state, reached either via `VMContext.machine_state`
//! (callers holding a `vmctx`) or via the per-thread [`CURRENT_MACHINE`] slot
//! (vmctx-less host fns and the external ambient shims).
//!
//! Homes all per-machine ambient state in one place, owned inline by each
//! `JitEffectMachine`: the external-cancellation flag, the JSON decode
//! constructor ids, the stack-map registry pointer, and the call-depth
//! counter (leaf 1); the first-cause runtime error and diagnostics (leaf 2);
//! and the GC state + GC root registries
//! (leaf 3: `GC_STATE`, `RUST_ROOTS`, `PERSISTENT_ROOTS`).
//! `install_registries` points the run's `VMContext.machine_state` at it and
//! installs it as this thread's [`CURRENT_MACHINE`].
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

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::context::VMContext;
use crate::host_fns::{GcState, RuntimeError};
use crate::stack_map::StackMapRegistry;

/// Per-machine ambient state. Each cell's wrapper type (`RefCell`/`Cell`) is
/// chosen to match the try_borrow/borrow-panic/take semantics its callers
/// rely on — see e.g. `set_first_cause`'s `try_borrow_mut` defense below.
pub struct MachineState {
    cancel_flag: RefCell<Option<Arc<AtomicBool>>>,
    json_con_ids: Cell<Option<tidepool_eval::json::JsonConIds>>,
    time_con_ids: Cell<Option<tidepool_eval::time::TimeConIds>>,
    stack_map_registry: RefCell<Option<*const StackMapRegistry>>,
    call_depth: Cell<u32>,
    runtime_error: RefCell<Option<RuntimeError>>,
    diagnostics: RefCell<Vec<String>>,
    /// Bumped once per actual collection (`perform_gc`). `deep_force` reads
    /// this to invalidate its address-keyed visited set whenever a GC could have
    /// relocated (or freed, then let something else reuse the address of)
    /// an object it recorded — an address-only check with no way to detect
    /// staleness would risk a false "already visited" hit after a
    /// collection reuses a since-vacated address for an unrelated object.
    gc_generation: Cell<u64>,
    gc_state: RefCell<Option<GcState>>,
    /// Run-scoped GC roots (`RUST_ROOTS`): heap-pointer slots registered by
    /// Rust host-fn frames the JIT frame walker cannot see. Cleared every
    /// `clear_run_scratch`/`clear_gc_state`.
    rust_roots: RefCell<Vec<*mut *mut u8>>,
    /// Session-scoped GC roots (`PERSISTENT_ROOTS`): tenured bindings'
    /// stable slots. Survive across runs; cleared only at machine teardown
    /// (`free_session_heap`).
    persistent_roots: RefCell<Vec<*mut *mut u8>>,
    /// STOWED GC roots (segment 40): the suspended continuation slot(s) of a
    /// parent turn parked at a typed yield (`runLLMTurn`/`Ask`), registered
    /// for the duration of a NESTED CHILD run so a child's collection evacuates
    /// the parent's stowed continuation tree instead of freeing it. Kept as a
    /// SEPARATE set from `persistent_roots` DELIBERATELY: intent must be
    /// auditable — a persistent root is a tenured persistent-binding-store binding that lives
    /// for the machine's whole life; a stowed root is a *transient* parent
    /// continuation rooted only while at least one child is running against the
    /// suspended machine. `perform_gc` folds this set in alongside the other
    /// three sources. Registered on entering nested-child mode, deregistered on
    /// parent resume or child teardown; cleared defensively at machine teardown
    /// (`free_session_heap`). NOT touched by `clear_run_scratch` — a child
    /// turn's per-run teardown must not strand the parent's continuation.
    stowed_roots: RefCell<Vec<*mut *mut u8>>,
    /// Write-barrier armed flag: false until `OldSpace::tenure` first runs.
    /// Before the first tenure there is no old-space, so no old-to-young
    /// store is possible — see `old_space.rs`'s module doc for the invariant.
    /// `host_fns::write_barrier` checks this FIRST and returns before any
    /// hashing/borrow when unarmed.
    write_barrier_armed: Cell<bool>,
    /// The write barrier's remembered set: slot addresses of every recorded
    /// old/external-to-young store (`host_fns::write_barrier`). A `HashSet`,
    /// not a `Vec` — the same slot can be re-targeted by repeated writes
    /// (e.g. a loop over `writeSmallArray#` on one index), and an unbounded
    /// `Vec` would grow without limit. `perform_gc` folds this into
    /// `root_slots` on every collection (both the initial Cheney pass and the
    /// doubling re-evacuate, since both reuse the same `root_slots` vector),
    /// so a remembered slot's target is evacuated and the slot rewritten in
    /// place exactly like any other root.
    remembered_slots: RefCell<HashSet<*mut *mut u8>>,
    /// Byte ranges of every currently-live old-space arena. `OldSpace` owns
    /// its arenas but hangs off `JitEffectMachine`/`SessionState`, not
    /// reachable from `perform_gc` (vmctx -> `MachineState` only) — recording
    /// each arena's range here as it is allocated gives a diagnostic pass
    /// old-space bounds without threading `OldSpace` itself through vmctx.
    old_space_arenas: RefCell<Vec<(*const u8, *const u8)>>,
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
            gc_generation: Cell::new(0),
            gc_state: RefCell::new(None),
            rust_roots: RefCell::new(Vec::new()),
            persistent_roots: RefCell::new(Vec::new()),
            stowed_roots: RefCell::new(Vec::new()),
            write_barrier_armed: Cell::new(false),
            remembered_slots: RefCell::new(HashSet::new()),
            old_space_arenas: RefCell::new(Vec::new()),
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
    /// Uses `try_borrow_mut` defensively: a fault + `siglongjmp` while
    /// something holds this cell mutably borrowed would leave it PERMANENTLY
    /// marked as mutably borrowed (a `RefCell` has no "unpoison"), and a
    /// plain `borrow_mut` on the signal-recovery path — inside unwind/cleanup
    /// — double-panics into `abort()` instead of surfacing
    /// `YieldError::Signal`. If the borrow fails we simply can't record this
    /// cause; silently dropping it (rather than panicking) is the same
    /// tradeoff `take_runtime_error` already makes.
    pub(crate) fn set_first_cause(&self, cause: RuntimeError) {
        if let Ok(mut slot) = self.runtime_error.try_borrow_mut() {
            if slot.is_none() {
                *slot = Some(cause);
            }
        }
    }

    /// Unconditionally overwrite the pending cause, rather than preserving
    /// an earlier one (unlike [`Self::set_first_cause`]'s first-write-wins).
    ///
    /// Same `try_borrow_mut` defense as [`Self::set_first_cause`] — this
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

    /// Same `try_borrow` defense as [`Self::take_runtime_error`]. Falls
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

    // --- GC generation counter --------------------------------------------

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

    // --- GC state (leaf 3) ------------------------------------------------
    // `set_gc_state`/`clear_gc_state` are `pub`: bare-VMContext test
    // harnesses (e.g. proptest_parked_registry.rs) own a MachineState, wire
    // `vmctx.machine_state` at it, and drive GC state directly — same
    // pattern leaf 1 used for `set_stack_map_registry`. The rest stay
    // `pub(crate)`.

    /// Set the active GC region for this machine.
    pub fn set_gc_state(&self, start: *mut u8, size: usize) {
        *self.gc_state.borrow_mut() = Some(GcState {
            active_start: start,
            active_size: size,
            active_buffer: None,
        });
    }

    /// Install a retained session heap buffer as the active GC region.
    pub(crate) fn install_session_buffer(&self, mut buffer: Vec<u64>) {
        let start = buffer.as_mut_ptr() as *mut u8;
        let size = buffer.len() * 8;
        *self.gc_state.borrow_mut() = Some(GcState {
            active_start: start,
            active_size: size,
            active_buffer: Some(buffer),
        });
    }

    /// Reclaim the live heap buffer + high-water cursor from this machine's
    /// GC state, called from `RegistryGuard::drop` BEFORE `clear_run_scratch`
    /// takes the `GcState`. Returns `(None, 0)` when there's no `GcState`
    /// installed (e.g. a run that never reached GC setup).
    pub(crate) fn reclaim_session_heap(&self, alloc_ptr: *mut u8) -> (Option<Vec<u64>>, usize) {
        match self.gc_state.borrow_mut().as_mut() {
            Some(state) => {
                let cursor = (alloc_ptr as usize).saturating_sub(state.active_start as usize);
                let buf = state.active_buffer.take();
                (buf, cursor)
            }
            None => (None, 0),
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
    /// teardown path.
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
        // Defensive: a machine dropped mid-nested-child (a child panicked and
        // its guard unwound) must not leave a dangling stowed slot registered.
        self.clear_stowed_roots();
        // Per-arena `retire_old_space_arena` calls (JitEffectMachine::drop,
        // before this runs) already forget slots pointing into old-space; this
        // is the blanket net for anything left (e.g. a boxed-array payload
        // slot, which lives in an external malloc'd buffer outside every
        // arena range).
        self.clear_remembered_slots();
        self.gc_state.borrow_mut().take();
    }

    /// Take this machine's `GcState` out of its cell, leaving the cell empty.
    /// `perform_gc` uses this to operate on an OWNED `GcState` across the
    /// Cheney copy instead of holding a live borrow across faultable code: a
    /// signal there abandons the owned value on the dead frame (it leaks,
    /// nothing double-frees) rather than leaving the `RefCell` permanently
    /// marked borrowed. Pair with [`Self::put_gc_state`].
    pub(crate) fn take_gc_state(&self) -> Option<GcState> {
        self.gc_state.borrow_mut().take()
    }

    /// Put a `GcState` previously removed by [`Self::take_gc_state`] back
    /// into the cell.
    pub(crate) fn put_gc_state(&self, state: GcState) {
        *self.gc_state.borrow_mut() = Some(state);
    }

    // --- rust roots (run-scoped GC roots, leaf 3) --------------------------

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
    /// `perform_gc` to build its root slot list.
    pub(crate) fn extend_rust_roots(&self, out: &mut Vec<*mut *mut u8>) {
        out.extend(self.rust_roots.borrow().iter().copied());
    }

    // --- persistent roots (session-scoped GC roots, leaf 3) ---------------

    pub(crate) fn register_persistent_root(&self, slot: *mut *mut u8) {
        self.persistent_roots.borrow_mut().push(slot);
    }

    /// Number of registered persistent roots (test/diagnostic accessor).
    pub(crate) fn persistent_roots_count(&self) -> usize {
        self.persistent_roots.borrow().len()
    }

    /// Deregister ONE persistent root by its slot address — the persistent
    /// sibling of [`Self::deregister_stowed_root`], with the same
    /// remove-by-position semantics (a slot registered once is removed once;
    /// an already-removed slot is a no-op, so release paths that can race a
    /// wholesale teardown stay idempotent). Added for per-runtime-resource-scope release
    /// (`JitEffectMachine::close_realm`): a released value's slot cell stays
    /// allocated (owned by `OldSpace::slots` for the machine's life — 8 bytes),
    /// but the GC stops tracing and rewriting it, so the value it pinned can
    /// be collected once nothing else reaches it.
    pub(crate) fn deregister_persistent_root(&self, slot: *mut *mut u8) {
        let mut roots = self.persistent_roots.borrow_mut();
        if let Some(pos) = roots.iter().position(|&s| s == slot) {
            roots.remove(pos);
        }
    }

    pub(crate) fn clear_persistent_roots(&self) {
        self.persistent_roots.borrow_mut().clear();
    }

    /// Append this machine's session-scoped persistent roots to `out` — the
    /// persistent-root sibling of `extend_rust_roots`, used by `perform_gc`.
    pub(crate) fn extend_persistent_roots(&self, out: &mut Vec<*mut *mut u8>) {
        out.extend(self.persistent_roots.borrow().iter().copied());
    }

    // --- stowed roots (nested-child-scoped GC roots, segment 40) ----------

    /// Register a STOWED GC root slot (segment 40): the parent's suspended
    /// continuation cell, rooted for the duration of a nested child run.
    ///
    /// Unlike a persistent root (session lifetime), a stowed root is
    /// deregistered when the parent resumes or the last child tears down.
    /// `perform_gc` folds these in on every collection, so a child's GC
    /// evacuates the parent's continuation tree and rewrites `*slot` in place.
    pub(crate) fn register_stowed_root(&self, slot: *mut *mut u8) {
        self.stowed_roots.borrow_mut().push(slot);
    }

    /// Remove a previously-registered stowed root by slot address (parent
    /// resume / child teardown). Removes the FIRST matching entry so nested
    /// child depth pairs each register with exactly one deregister.
    pub(crate) fn deregister_stowed_root(&self, slot: *mut *mut u8) {
        let mut roots = self.stowed_roots.borrow_mut();
        if let Some(pos) = roots.iter().position(|&s| s == slot) {
            roots.remove(pos);
        }
    }

    /// Number of registered stowed roots (test/diagnostic accessor).
    pub(crate) fn stowed_roots_count(&self) -> usize {
        self.stowed_roots.borrow().len()
    }

    /// Clear all stowed roots (defensive machine-teardown path).
    pub(crate) fn clear_stowed_roots(&self) {
        self.stowed_roots.borrow_mut().clear();
    }

    /// Append this machine's stowed roots to `out` — the stowed-root sibling of
    /// `extend_persistent_roots`, used by `perform_gc`.
    pub(crate) fn extend_stowed_roots(&self, out: &mut Vec<*mut *mut u8>) {
        out.extend(self.stowed_roots.borrow().iter().copied());
    }

    // --- write barrier / remembered set (generational write barrier) ------

    /// Arm the barrier. Idempotent; `OldSpace::tenure` calls this unconditionally
    /// on every tenure (cheap even when already armed).
    pub(crate) fn arm_write_barrier(&self) {
        self.write_barrier_armed.set(true);
    }

    /// Whether the barrier is armed — the cheap disarmed-check
    /// `host_fns::write_barrier` reads before any hashing or borrow.
    pub(crate) fn write_barrier_armed(&self) -> bool {
        self.write_barrier_armed.get()
    }

    /// Record `slot` in the remembered set.
    pub(crate) fn register_remembered_slot(&self, slot: *mut *mut u8) {
        self.remembered_slots.borrow_mut().insert(slot);
    }

    /// Number of remembered slots (test/diagnostic accessor).
    pub(crate) fn remembered_slots_count(&self) -> usize {
        self.remembered_slots.borrow().len()
    }

    /// Clear all remembered slots (machine-teardown path, alongside
    /// `clear_persistent_roots`).
    pub(crate) fn clear_remembered_slots(&self) {
        self.remembered_slots.borrow_mut().clear();
    }

    /// Append this machine's remembered slots to `out` — the remembered-set
    /// sibling of `extend_stowed_roots`, used by `perform_gc`.
    pub(crate) fn extend_remembered_slots(&self, out: &mut Vec<*mut *mut u8>) {
        out.extend(self.remembered_slots.borrow().iter().copied());
    }

    /// Snapshot of every currently-remembered slot. Read-only; does not
    /// affect GC. Read by `host_fns::gc`'s post-GC `verify_remembered_slots`
    /// pass under `TIDEPOOL_HEAP_VERIFY`.
    pub(crate) fn remembered_slots_snapshot(&self) -> Vec<*mut *mut u8> {
        self.remembered_slots.borrow().iter().copied().collect()
    }

    /// Remove every remembered slot whose address falls in `[start, end)`.
    /// Invariant: no remembered slot outlives the memory it points into.
    pub(crate) fn forget_remembered_range(&self, start: *const u8, end: *const u8) {
        let (s, e) = (start as usize, end as usize);
        self.remembered_slots.borrow_mut().retain(|&slot| {
            let a = slot as usize;
            a < s || a >= e
        });
    }

    // --- old-space arena ranges (diagnostic reach) -------------------------

    /// Register a newly-allocated old-space arena's byte range.
    pub(crate) fn register_old_space_arena(&self, start: *const u8, end: *const u8) {
        self.old_space_arenas.borrow_mut().push((start, end));
    }

    /// Retire an old-space arena: forget any remembered slot pointing into
    /// `[start, end)` and deregister the range itself. Call exactly once per
    /// arena, at the point its backing memory is about to be freed, so a
    /// stale range is never read as live old-space.
    pub(crate) fn retire_old_space_arena(&self, start: *const u8, end: *const u8) {
        self.forget_remembered_range(start, end);
        self.old_space_arenas
            .borrow_mut()
            .retain(|&(s, e)| !(s == start && e == end));
    }

    /// Snapshot of every currently-live old-space arena's byte range, for a
    /// diagnostic verifier pass that needs old-space bounds and can only
    /// reach `MachineState` (via vmctx), not `OldSpace` itself.
    pub(crate) fn old_space_arena_ranges(&self) -> Vec<(*const u8, *const u8)> {
        self.old_space_arenas.borrow().iter().copied().collect()
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
    /// `push_diagnostic`/`drain_diagnostics`). Each eval runs on its own
    /// dedicated thread
    /// (server.rs spawns one per eval, up to `MAX_CONCURRENT_EVALS`
    /// concurrently, and a suspended eval keeps its thread + machine +
    /// `RegistryGuard` alive across the suspension) — so per-thread reach is
    /// per-eval reach, which is the correctness this cell must preserve. A
    /// process-global slot would be a cancellation regression here: two
    /// machines CAN be live at once, and a global pointer would let one
    /// eval's abort land on another's machine.
    ///
    /// State itself still lives on [`MachineState`], owned per-machine; this
    /// cell is only the reach path for code that has no `vmctx` to follow.
    /// Never used for the GC cluster (leaf 3) — see the module-level
    /// "GC-cluster reach" note for why a per-thread slot can't stand in for
    /// `vmctx` there.
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

    /// `runtime_error` relies on `try_borrow_mut` defenses: a fault +
    /// `siglongjmp` while something holds it mutably borrowed would leave it
    /// PERMANENTLY marked as mutably borrowed (`RefCell` has no "unpoison"
    /// once a guard's release never runs). We reproduce that exact `RefCell`
    /// state directly — hold a live `borrow_mut()` guard across the calls
    /// under test — rather than actually raising a signal; `signal_safety.rs`
    /// separately covers signal delivery/recovery itself. `gc_state` avoids
    /// this hazard class entirely via a take/put-back discipline instead —
    /// see the `gc_state_take_put_back_*` tests below.
    #[test]
    fn stuck_runtime_error_cell_does_not_panic() {
        let ms = MachineState::new();
        let _guard = ms.runtime_error.borrow_mut(); // simulates a stuck signal-path borrow

        // has_runtime_error: conservative `true` fallback, not a panic.
        assert!(ms.has_runtime_error());
        // set_first_cause / set_runtime_error_overwrite: silently no-op, not a panic.
        ms.set_first_cause(RuntimeError::Cancelled);
        ms.set_runtime_error_overwrite(RuntimeError::Cancelled);
        // take_runtime_error: None, not a panic.
        assert_eq!(ms.take_runtime_error(), None);
    }

    #[test]
    fn gc_state_take_put_back_round_trips() {
        let ms = MachineState::new();
        ms.set_gc_state(std::ptr::dangling_mut(), 128);

        let state = ms.take_gc_state();
        assert!(state.is_some());
        assert!(
            ms.gc_active_range().is_none(),
            "cell must be empty while the state is out"
        );

        ms.put_gc_state(state.unwrap());
        assert!(
            ms.gc_active_range().is_some(),
            "put_gc_state must restore the cell"
        );
    }

    /// Simulates a fault mid-`perform_gc`: the `GcState` is taken out and the
    /// frame holding it is abandoned (a `siglongjmp` skips the put-back). The
    /// cell is left EMPTY rather than stuck mutably-borrowed, so every
    /// teardown path that runs during signal recovery — including
    /// `clear_run_scratch`, called from `RegistryGuard::drop` — completes
    /// without panicking.
    #[test]
    fn gc_state_abandoned_take_leaves_cell_empty_and_teardown_is_safe() {
        let ms = MachineState::new();
        ms.set_gc_state(std::ptr::dangling_mut(), 128);

        let _abandoned = ms.take_gc_state(); // never put back

        assert_eq!(ms.reclaim_session_heap(std::ptr::null_mut()), (None, 0));
        ms.clear_run_scratch();
        ms.free_session_heap();
    }
}
