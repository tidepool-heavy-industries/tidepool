//! Per-machine ambient state, reached either via `VMContext.machine_state`
//! (callers holding a `vmctx`) or via the per-thread [`CURRENT_MACHINE`] slot
//! (vmctx-less host fns and the external ambient shims).
//!
//! Homes all per-machine ambient state in one place, owned inline by each
//! `JitEffectMachine`: the external-cancellation flag, the JSON decode
//! constructor ids, the stack-map registry pointer, and the call-depth
//! counter (leaf 1); the first-cause runtime error and diagnostics (leaf 2);
//! the GC state + GC root registries (leaf 3: `GC_STATE`, `RUST_ROOTS`,
//! `PERSISTENT_ROOTS`); and the external payload ledger (leaf 4).
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
//! External allocation is the narrow exception to the vmctx-only GC reach:
//! the existing byte/boxed allocation ABIs have no vmctx argument, so they
//! register through `CURRENT_MACHINE` before initializing or returning the
//! payload. `RegistryGuard` installs that pointer from the same
//! `JitEffectMachine::machine_state` placed in vmctx, and restores it before
//! another machine can run on the thread. Collection reaches the ledger back
//! through vmctx; `MachineState::drop` reaches it directly. Thus allocation,
//! tracing/sweep, and teardown converge on the same owner without a process
//! registry or shared scratch allocation.
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

use std::alloc::Layout;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::context::VMContext;
use crate::host_fns::{GcState, RuntimeError};
use crate::stack_map::StackMapRegistry;

/// Whether another entry may safely reuse this machine after a failed run.
///
/// `Unavailable` is monotonic for the lifetime of a machine: once execution
/// observes an integrity failure, later cleanup cannot prove that compiled
/// state, heap roots, and external storage are mutually consistent again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineDisposition {
    Reusable,
    Unavailable,
}

/// The retained first cause and the reuse decision it imposed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineFailure {
    pub cause: RuntimeError,
    pub disposition: MachineDisposition,
}

/// The two GC-external payload shapes owned by a machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalStorageKind {
    Bytes,
    BoxedArray,
}

/// Lifetime accounting for a machine's GC-external payloads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExternalStorageStats {
    pub allocated_bytes: usize,
    pub allocated_objects: usize,
    pub live_bytes: usize,
    pub live_objects: usize,
    pub freed_bytes: usize,
    pub freed_objects: usize,
}

/// One complete, point-in-time set of slots that collection must trace and
/// rewrite. Remembered edges stay separately classified: minor collection
/// consumes them as roots, while major collection reaches them through their
/// live old/external owners rather than letting a dead owner root itself.
///
/// The constructor is private to [`MachineState::complete_root_snapshot`], so
/// collectors cannot accidentally select only one registry. Stack roots are
/// supplied by the checked frame walk; every ambient machine registry and the
/// two VM tail-call slots are joined here in one owning entry point.
pub(crate) struct GcRootSnapshot {
    slots: Vec<*mut *mut u8>,
    remembered_slots: Vec<*mut *mut u8>,
}

impl GcRootSnapshot {
    pub(crate) fn into_slots(mut self) -> Vec<*mut *mut u8> {
        self.slots.append(&mut self.remembered_slots);
        self.slots
    }

    /// Strong roots for a full graph trace. Remembered slots describe edges
    /// from old/external owners; they are not independent roots when deciding
    /// whether those owners themselves are live.
    pub(crate) fn into_major_slots(self) -> Vec<*mut *mut u8> {
        self.slots
    }
}

struct ExternalStorage {
    base: *mut u8,
    layout: Layout,
    #[allow(
        dead_code,
        reason = "consumed by the independently integrated major collector"
    )]
    kind: ExternalStorageKind,
    logical_len: usize,
}

#[allow(
    dead_code,
    reason = "consumed by the independently integrated major collector"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExternalStorageValidationError {
    Untracked(*mut u8),
    InvalidBase,
    LayoutAlignment {
        actual: usize,
    },
    PointerAlignment {
        kind: ExternalStorageKind,
    },
    KindMismatch {
        expected: ExternalStorageKind,
        actual: ExternalStorageKind,
    },
    PublishedPointerMismatch {
        kind: ExternalStorageKind,
    },
    SpanOverflow {
        kind: ExternalStorageKind,
        logical_len: usize,
    },
    SpanExceedsAllocation {
        kind: ExternalStorageKind,
        required: usize,
        allocated: usize,
    },
    CapacityPrefixMismatch {
        recorded: usize,
        stored: usize,
    },
    LogicalLengthMismatch {
        kind: ExternalStorageKind,
        recorded: usize,
        stored: usize,
    },
    LedgerChanged,
}

/// Validated pointer-bearing slots in one tracked external payload.
#[allow(
    dead_code,
    reason = "consumed by the independently integrated major collector"
)]
pub(crate) struct ExternalPayloadView {
    pub(crate) pointer_slots: Vec<*mut *mut u8>,
}

/// Allocation-bearing sweep plan produced before a major collector commits.
/// Fields remain private so callers cannot fabricate a partial dead set.
#[allow(
    dead_code,
    reason = "consumed by the independently integrated major collector"
)]
pub(crate) struct ExternalSweepPlan {
    allocated_objects: usize,
    live_objects: usize,
    dead: Vec<*mut u8>,
}

/// Per-machine ambient state. Each cell's wrapper type (`RefCell`/`Cell`) is
/// chosen to match the try_borrow/borrow-panic/take semantics its callers
/// rely on — see e.g. `set_first_cause`'s `try_borrow_mut` defense below.
pub struct MachineState {
    cancel_flag: RefCell<Option<Arc<AtomicBool>>>,
    text_con_id: Cell<Option<tidepool_repr::DataConId>>,
    json_con_ids: Cell<Option<tidepool_bridge::json_builder::JsonConIds>>,
    time_con_ids: Cell<Option<tidepool_bridge::time::TimeConIds>>,
    stack_map_registry: RefCell<Option<*const StackMapRegistry>>,
    call_depth: Cell<u32>,
    runtime_error: RefCell<Option<RuntimeError>>,
    disposition: Cell<MachineDisposition>,
    last_failure: RefCell<Option<MachineFailure>>,
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
    /// Stable slots embedded as loads in finalized session fragments. A
    /// binding may leave the session table while older callable code still
    /// names its slot, so these remain strong for the machine/code lifetime.
    code_roots: RefCell<HashSet<*mut *mut u8>>,
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
    /// Payloads allocated outside the moving heap. The map key is the pointer
    /// published in a Lit's value word; `base` may differ for byte arrays,
    /// whose ABI pointer follows a hidden allocation-size word.
    external_storage: RefCell<HashMap<*mut u8, ExternalStorage>>,
    external_allocated_bytes: Cell<usize>,
    external_allocated_objects: Cell<usize>,
    external_freed_bytes: Cell<usize>,
    external_freed_objects: Cell<usize>,
}

// SAFETY: MachineState is only ever accessed from the single thread driving
// the owning JitEffectMachine's run; the raw stack-map pointer is never
// dereferenced off that thread.
unsafe impl Send for MachineState {}

impl MachineState {
    pub fn new() -> Self {
        Self {
            cancel_flag: RefCell::new(None),
            text_con_id: Cell::new(None),
            json_con_ids: Cell::new(None),
            time_con_ids: Cell::new(None),
            stack_map_registry: RefCell::new(None),
            call_depth: Cell::new(0),
            runtime_error: RefCell::new(None),
            disposition: Cell::new(MachineDisposition::Reusable),
            last_failure: RefCell::new(None),
            diagnostics: RefCell::new(Vec::new()),
            gc_generation: Cell::new(0),
            gc_state: RefCell::new(None),
            rust_roots: RefCell::new(Vec::new()),
            persistent_roots: RefCell::new(Vec::new()),
            stowed_roots: RefCell::new(Vec::new()),
            code_roots: RefCell::new(HashSet::new()),
            write_barrier_armed: Cell::new(false),
            remembered_slots: RefCell::new(HashSet::new()),
            old_space_arenas: RefCell::new(Vec::new()),
            external_storage: RefCell::new(HashMap::new()),
            external_allocated_bytes: Cell::new(0),
            external_allocated_objects: Cell::new(0),
            external_freed_bytes: Cell::new(0),
            external_freed_objects: Cell::new(0),
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

    // --- Host-built value constructor ids ----------------------------------

    pub(crate) fn set_text_con_id(&self, id: Option<tidepool_repr::DataConId>) {
        self.text_con_id.set(id);
    }

    pub(crate) fn text_con_id(&self) -> Option<tidepool_repr::DataConId> {
        self.text_con_id.get()
    }

    pub(crate) fn set_json_con_ids(&self, ids: Option<tidepool_bridge::json_builder::JsonConIds>) {
        self.json_con_ids.set(ids);
    }

    pub(crate) fn json_con_ids(&self) -> Option<tidepool_bridge::json_builder::JsonConIds> {
        self.json_con_ids.get()
    }

    pub(crate) fn set_time_con_ids(&self, ids: Option<tidepool_bridge::time::TimeConIds>) {
        self.time_con_ids.set(ids);
    }

    pub(crate) fn time_con_ids(&self) -> Option<tidepool_bridge::time::TimeConIds> {
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
        let disposition = cause.machine_disposition();
        if disposition == MachineDisposition::Unavailable {
            self.disposition.set(MachineDisposition::Unavailable);
        }
        if let Ok(mut slot) = self.runtime_error.try_borrow_mut() {
            if slot.is_none() {
                *slot = Some(cause.clone());
                if let Ok(mut failure) = self.last_failure.try_borrow_mut() {
                    *failure = Some(MachineFailure {
                        cause,
                        disposition: self.disposition.get(),
                    });
                }
            }
        }
        // A later integrity observation cannot replace the first cause, but
        // it still makes reuse unsafe. Keep the retained cause and upgrade
        // its disposition to match the machine's monotonic decision.
        if disposition == MachineDisposition::Unavailable {
            if let Ok(mut failure) = self.last_failure.try_borrow_mut() {
                if let Some(failure) = failure.as_mut() {
                    failure.disposition = MachineDisposition::Unavailable;
                }
            }
        }
    }

    pub(crate) fn disposition(&self) -> MachineDisposition {
        self.disposition.get()
    }

    pub(crate) fn last_failure(&self) -> Option<MachineFailure> {
        self.last_failure.try_borrow().ok().and_then(|f| f.clone())
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

    /// Inspect the first cause without consuming it at an emitted ABI boundary.
    pub(crate) fn prepared_call_status(&self) -> crate::prepared_control::CallStatus {
        use crate::prepared_control::CallStatus;
        if self.disposition() == MachineDisposition::Unavailable {
            return CallStatus::IntegrityFailure;
        }
        match self.runtime_error.try_borrow() {
            Ok(cause) => match cause.as_ref() {
                None => CallStatus::Success,
                Some(RuntimeError::Cancelled) => CallStatus::Cancelled,
                Some(error) if error.machine_disposition() == MachineDisposition::Unavailable => {
                    CallStatus::IntegrityFailure
                }
                Some(_) => CallStatus::LanguageFailure,
            },
            Err(_) => CallStatus::IntegrityFailure,
        }
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
            prepared: None,
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
            prepared: None,
        });
    }

    /// Install one prepared heap with its pinned compiled-layout owners.
    /// A live heap must be retired by its owning run before another is installed.
    pub(crate) fn install_prepared_buffer(
        &self,
        buffer: Vec<u64>,
        layouts: Vec<std::sync::Arc<tidepool_heap::execution_descriptor::ObjectDescriptor>>,
    ) -> Result<(), RuntimeError> {
        self.install_prepared_buffer_with_static_region(buffer, layouts, None)
    }

    /// Install a prepared nursery whose descriptor space admits one immutable
    /// invocation-owned static region as an external managed space.
    pub(crate) fn install_prepared_buffer_with_static_region(
        &self,
        mut buffer: Vec<u64>,
        layouts: Vec<std::sync::Arc<tidepool_heap::execution_descriptor::ObjectDescriptor>>,
        static_region: Option<Arc<tidepool_heap::static_region::StaticRegion>>,
    ) -> Result<(), RuntimeError> {
        let mut active = self
            .gc_state
            .try_borrow_mut()
            .map_err(|_| RuntimeError::BadPointer)?;
        if active.is_some() {
            return Err(RuntimeError::BadPointer);
        }
        let space = tidepool_heap::gc::raw::DescriptorSpace::new(layouts.iter().cloned())
            .map_err(|_| RuntimeError::HeapOverflow)?;
        let space = if let Some(region) = static_region {
            space.with_static_region(region)
        } else {
            space
        };
        *active = Some(GcState {
            active_start: buffer.as_mut_ptr().cast(),
            active_size: std::mem::size_of_val(buffer.as_slice()),
            active_buffer: Some(buffer),
            prepared: Some(crate::host_fns::PreparedHeap {
                space,
                spare: Vec::new(),
                used: 0,
            }),
        });
        Ok(())
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

    pub(crate) fn register_code_roots(&self, roots: impl IntoIterator<Item = *mut *mut u8>) {
        self.code_roots.borrow_mut().extend(roots);
    }

    pub(crate) fn extend_code_roots(&self, out: &mut Vec<*mut *mut u8>) {
        out.extend(self.code_roots.borrow().iter().copied());
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

    /// Join a successful generated-frame walk with every ambient root registry.
    ///
    /// `tail_callee_slot` and `tail_arg_slot` are stable fields in the live
    /// `VMContext`. They are omitted only when their current value is null,
    /// preserving the existing collector behavior. Other categories retain
    /// null-valued slots: the slot itself is stable and a later collection may
    /// need to rewrite it after its owner fills the value.
    ///
    /// This method does not make an unsuccessful frame walk complete. Its caller
    /// must construct the stack-root slice only after `walk_frames` succeeds;
    /// the opaque return type then prevents downstream collectors from rebuilding
    /// a partial registry list.
    pub(crate) unsafe fn complete_root_snapshot(
        &self,
        stack_roots: &[*mut *mut u8],
        tail_callee_slot: *mut *mut u8,
        tail_arg_slot: *mut *mut u8,
    ) -> GcRootSnapshot {
        let mut slots = Vec::with_capacity(
            stack_roots.len()
                + self.rust_roots.borrow().len()
                + self.persistent_roots.borrow().len()
                + self.stowed_roots.borrow().len()
                + self.code_roots.borrow().len()
                + self.remembered_slots.borrow().len()
                + 2,
        );
        slots.extend_from_slice(stack_roots);
        self.extend_rust_roots(&mut slots);
        self.extend_persistent_roots(&mut slots);
        self.extend_stowed_roots(&mut slots);
        self.extend_code_roots(&mut slots);
        let mut remembered_slots = Vec::with_capacity(self.remembered_slots.borrow().len());
        self.extend_remembered_slots(&mut remembered_slots);

        // SAFETY: the caller supplies valid VMContext field addresses.
        if !tail_callee_slot.is_null() && !unsafe { *tail_callee_slot }.is_null() {
            slots.push(tail_callee_slot);
        }
        // SAFETY: the caller supplies valid VMContext field addresses.
        if !tail_arg_slot.is_null() && !unsafe { *tail_arg_slot }.is_null() {
            slots.push(tail_arg_slot);
        }

        GcRootSnapshot {
            slots,
            remembered_slots,
        }
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

    // --- GC-external byte/reference storage -------------------------------

    /// Take ownership of a fresh allocation before its pointer is initialized
    /// or published to JIT code.
    pub(crate) fn register_external_storage(
        &self,
        published: *mut u8,
        base: *mut u8,
        layout: Layout,
        kind: ExternalStorageKind,
        logical_len: usize,
    ) {
        let old = self.external_storage.borrow_mut().insert(
            published,
            ExternalStorage {
                base,
                layout,
                kind,
                logical_len,
            },
        );
        debug_assert!(
            old.is_none(),
            "external allocation pointer registered twice"
        );
        self.external_allocated_bytes.set(
            self.external_allocated_bytes
                .get()
                .saturating_add(layout.size()),
        );
        self.external_allocated_objects
            .set(self.external_allocated_objects.get().saturating_add(1));
    }

    pub(crate) fn set_external_logical_len(&self, ptr: *mut u8, logical_len: usize) {
        if let Some(record) = self.external_storage.borrow_mut().get_mut(&ptr) {
            record.logical_len = logical_len;
        }
    }

    #[allow(
        dead_code,
        reason = "consumed by the independently integrated major collector"
    )]
    fn validate_external_record(
        published: *mut u8,
        record: &ExternalStorage,
    ) -> Result<(), ExternalStorageValidationError> {
        let prefix_alignment = std::mem::align_of::<u64>();
        if record.base.is_null() {
            return Err(ExternalStorageValidationError::InvalidBase);
        }
        if record.layout.align() < prefix_alignment {
            return Err(ExternalStorageValidationError::LayoutAlignment {
                actual: record.layout.align(),
            });
        }
        if (record.base as usize) % prefix_alignment != 0
            || (published as usize) % prefix_alignment != 0
        {
            return Err(ExternalStorageValidationError::PointerAlignment { kind: record.kind });
        }
        let stored_len = match record.kind {
            ExternalStorageKind::Bytes => {
                // Byte arrays publish eight bytes after their allocation base:
                // [capacity][logical length][bytes...].
                let expected_published = (record.base as usize).checked_add(8).ok_or(
                    ExternalStorageValidationError::SpanOverflow {
                        kind: record.kind,
                        logical_len: record.logical_len,
                    },
                )?;
                if published as usize != expected_published {
                    return Err(ExternalStorageValidationError::PublishedPointerMismatch {
                        kind: record.kind,
                    });
                }
                let required = 16usize.checked_add(record.logical_len).ok_or(
                    ExternalStorageValidationError::SpanOverflow {
                        kind: record.kind,
                        logical_len: record.logical_len,
                    },
                )?;
                if required > record.layout.size() {
                    return Err(ExternalStorageValidationError::SpanExceedsAllocation {
                        kind: record.kind,
                        required,
                        allocated: record.layout.size(),
                    });
                }
                // SAFETY: pointer relationship and required allocation span
                // were checked above before either prefix is read.
                let stored_capacity = unsafe { *(record.base as *const u64) } as usize;
                if stored_capacity != record.layout.size() {
                    return Err(ExternalStorageValidationError::CapacityPrefixMismatch {
                        recorded: record.layout.size(),
                        stored: stored_capacity,
                    });
                }
                // SAFETY: required >= 16, so the published length prefix is
                // within the registered allocation.
                (unsafe { *(published as *const u64) }) as usize
            }
            ExternalStorageKind::BoxedArray => {
                if published != record.base {
                    return Err(ExternalStorageValidationError::PublishedPointerMismatch {
                        kind: record.kind,
                    });
                }
                let required = record
                    .logical_len
                    .checked_mul(std::mem::size_of::<*mut u8>())
                    .and_then(|bytes| 8usize.checked_add(bytes))
                    .ok_or(ExternalStorageValidationError::SpanOverflow {
                        kind: record.kind,
                        logical_len: record.logical_len,
                    })?;
                if required > record.layout.size() {
                    return Err(ExternalStorageValidationError::SpanExceedsAllocation {
                        kind: record.kind,
                        required,
                        allocated: record.layout.size(),
                    });
                }
                // SAFETY: required >= 8, so the length prefix is contained.
                (unsafe { *(published as *const u64) }) as usize
            }
        };
        if stored_len != record.logical_len {
            return Err(ExternalStorageValidationError::LogicalLengthMismatch {
                kind: record.kind,
                recorded: record.logical_len,
                stored: stored_len,
            });
        }
        Ok(())
    }

    /// Validate a wrapper's published payload identity before exposing boxed
    /// reference slots to graph traversal. Byte payloads yield no heap edges.
    #[allow(
        dead_code,
        reason = "consumed by the independently integrated major collector"
    )]
    pub(crate) fn external_payload_view(
        &self,
        published: *mut u8,
        expected: ExternalStorageKind,
    ) -> Result<ExternalPayloadView, ExternalStorageValidationError> {
        let storage = self.external_storage.borrow();
        let record = storage
            .get(&published)
            .ok_or(ExternalStorageValidationError::Untracked(published))?;
        if record.kind != expected {
            return Err(ExternalStorageValidationError::KindMismatch {
                expected,
                actual: record.kind,
            });
        }
        Self::validate_external_record(published, record)?;
        let pointer_slots = if record.kind == ExternalStorageKind::BoxedArray {
            (0..record.logical_len)
                // SAFETY: validation proved the complete slot span is inside
                // the registered allocation.
                .map(|index| unsafe { published.add(8 + index * 8) as *mut *mut u8 })
                .collect()
        } else {
            Vec::new()
        };
        Ok(ExternalPayloadView { pointer_slots })
    }

    /// Validate wrapper-declared payloads and remember every boxed element
    /// slot. This is called when wrappers first move into old-space, before
    /// the tenure fixup minor collection, so initialization performed while
    /// the barrier was disarmed cannot strand a nursery child.
    pub(crate) fn remember_external_payload_edges(
        &self,
        payloads: impl IntoIterator<Item = (*mut u8, ExternalStorageKind)>,
    ) -> Result<(), ExternalStorageValidationError> {
        for (published, kind) in payloads {
            for slot in self.external_payload_view(published, kind)?.pointer_slots {
                self.register_remembered_slot(slot);
            }
        }
        Ok(())
    }

    /// Validate the complete ledger and stage the exact unmarked allocation
    /// identities. Staging allocates; commit below does not.
    #[allow(
        dead_code,
        reason = "consumed by the independently integrated major collector"
    )]
    pub(crate) fn plan_external_sweep(
        &self,
        marked: &HashSet<*mut u8>,
    ) -> Result<ExternalSweepPlan, ExternalStorageValidationError> {
        let storage = self.external_storage.borrow();
        for &published in marked {
            if !storage.contains_key(&published) {
                return Err(ExternalStorageValidationError::Untracked(published));
            }
        }
        for (&published, record) in storage.iter() {
            Self::validate_external_record(published, record)?;
        }
        let dead = storage
            .keys()
            .copied()
            .filter(|pointer| !marked.contains(pointer))
            .collect();
        Ok(ExternalSweepPlan {
            allocated_objects: self.external_allocated_objects.get(),
            live_objects: storage.len(),
            dead,
        })
    }

    /// Commit a previously validated sweep without allocating. The major
    /// collector holds exclusive machine access between plan and commit; the
    /// counters still fence accidental stale-plan reuse before any mutation.
    #[allow(
        dead_code,
        reason = "consumed by the independently integrated major collector"
    )]
    pub(crate) fn commit_external_sweep(
        &self,
        plan: ExternalSweepPlan,
    ) -> Result<ExternalStorageStats, ExternalStorageValidationError> {
        if self.external_allocated_objects.get() != plan.allocated_objects
            || self.external_storage.borrow().len() != plan.live_objects
            || !plan
                .dead
                .iter()
                .all(|pointer| self.external_storage.borrow().contains_key(pointer))
        {
            return Err(ExternalStorageValidationError::LedgerChanged);
        }
        for pointer in plan.dead {
            let removed = self.release_external_storage(pointer);
            debug_assert!(removed, "validated external sweep entry disappeared");
        }
        Ok(self.external_storage_stats())
    }

    /// Release one allocation, first retiring remembered slots located in it.
    pub(crate) fn release_external_storage(&self, ptr: *mut u8) -> bool {
        let Some(record) = self.external_storage.borrow_mut().remove(&ptr) else {
            return false;
        };
        let start = record.base as *const u8;
        // SAFETY: `layout` is the exact allocation layout registered with
        // `base`, so this address computation stays within/one-past it.
        let end = unsafe { record.base.add(record.layout.size()) as *const u8 };
        self.forget_remembered_range(start, end);
        // SAFETY: ownership was removed above, making this the sole dealloc.
        unsafe { std::alloc::dealloc(record.base, record.layout) };
        self.external_freed_bytes.set(
            self.external_freed_bytes
                .get()
                .saturating_add(record.layout.size()),
        );
        self.external_freed_objects
            .set(self.external_freed_objects.get().saturating_add(1));
        true
    }

    fn release_all_external_storage(&self) {
        let pointers: Vec<_> = self.external_storage.borrow().keys().copied().collect();
        for ptr in pointers {
            self.release_external_storage(ptr);
        }
    }

    pub fn external_storage_stats(&self) -> ExternalStorageStats {
        let live = self.external_storage.borrow();
        ExternalStorageStats {
            allocated_bytes: self.external_allocated_bytes.get(),
            allocated_objects: self.external_allocated_objects.get(),
            live_bytes: live.values().map(|record| record.layout.size()).sum(),
            live_objects: live.len(),
            freed_bytes: self.external_freed_bytes.get(),
            freed_objects: self.external_freed_objects.get(),
        }
    }
}

impl Drop for MachineState {
    fn drop(&mut self) {
        self.release_all_external_storage();
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
        // set_first_cause: silently cannot write the cause, but does not panic.
        ms.set_first_cause(RuntimeError::Cancelled);
        // take_runtime_error: None, not a panic.
        assert_eq!(ms.take_runtime_error(), None);
    }

    #[test]
    fn first_cause_wins_while_integrity_disposition_is_monotonic() {
        let ms = MachineState::new();
        ms.set_first_cause(RuntimeError::Cancelled);
        ms.set_first_cause(RuntimeError::BadPointer);

        assert_eq!(ms.take_runtime_error(), Some(RuntimeError::Cancelled));
        assert_eq!(ms.disposition(), MachineDisposition::Unavailable);
        assert_eq!(
            ms.last_failure(),
            Some(MachineFailure {
                cause: RuntimeError::Cancelled,
                disposition: MachineDisposition::Unavailable,
            })
        );
    }

    #[test]
    fn ordinary_language_failure_remains_reusable() {
        let ms = MachineState::new();
        ms.set_first_cause(RuntimeError::UserErrorMsg("boom".into()));

        assert_eq!(ms.disposition(), MachineDisposition::Reusable);
        assert_eq!(
            ms.take_runtime_error(),
            Some(RuntimeError::UserErrorMsg("boom".into()))
        );
    }

    #[test]
    fn complete_root_snapshot_joins_every_registry_and_live_tail_slot() {
        let ms = MachineState::new();
        let mut stack_value = 1usize as *mut u8;
        let mut rust_value = 2usize as *mut u8;
        let mut persistent_value = 3usize as *mut u8;
        let mut stowed_value = 4usize as *mut u8;
        let mut remembered_value = 5usize as *mut u8;
        let mut code_value = 6usize as *mut u8;
        let mut tail_callee = 7usize as *mut u8;
        let mut tail_arg = std::ptr::null_mut();

        ms.register_rust_root(&mut rust_value);
        ms.register_persistent_root(&mut persistent_value);
        ms.register_stowed_root(&mut stowed_value);
        ms.register_code_roots([&mut code_value as *mut *mut u8]);
        ms.register_remembered_slot(&mut remembered_value);

        // SAFETY: every argument is the stable address of a live local pointer
        // slot for the duration of this assertion.
        let slots = unsafe {
            ms.complete_root_snapshot(&[&mut stack_value], &mut tail_callee, &mut tail_arg)
        }
        .into_slots();

        assert_eq!(slots.len(), 7);
        for expected in [
            &mut stack_value as *mut *mut u8,
            &mut rust_value,
            &mut persistent_value,
            &mut stowed_value,
            &mut code_value,
            &mut remembered_value,
            &mut tail_callee,
        ] {
            assert!(slots.contains(&expected));
        }
        assert!(!slots.contains(&(&mut tail_arg as *mut *mut u8)));
    }

    #[test]
    fn major_snapshot_keeps_remembered_edges_distinct_from_strong_roots() {
        let ms = MachineState::new();
        let mut persistent = 1usize as *mut u8;
        let mut remembered = 2usize as *mut u8;
        ms.register_persistent_root(&mut persistent);
        ms.register_remembered_slot(&mut remembered);

        let slots =
            unsafe { ms.complete_root_snapshot(&[], std::ptr::null_mut(), std::ptr::null_mut()) }
                .into_major_slots();
        assert_eq!(slots, vec![&mut persistent as *mut *mut u8]);
        assert!(!slots.contains(&(&mut remembered as *mut *mut u8)));
    }

    unsafe fn register_test_external(
        ms: &MachineState,
        kind: ExternalStorageKind,
        logical_len: usize,
    ) -> *mut u8 {
        let (total, published_offset) = match kind {
            ExternalStorageKind::Bytes => (16 + logical_len, 8),
            ExternalStorageKind::BoxedArray => (8 + logical_len * 8, 0),
        };
        let layout = Layout::from_size_align(total, 8).unwrap();
        // SAFETY: test layout is valid and nonempty.
        let base = unsafe { std::alloc::alloc_zeroed(layout) };
        assert!(!base.is_null());
        // SAFETY: published_offset is within the allocation.
        let published = unsafe { base.add(published_offset) };
        match kind {
            ExternalStorageKind::Bytes => {
                // SAFETY: the allocation contains both prefixes.
                unsafe {
                    *(base as *mut u64) = total as u64;
                    *(published as *mut u64) = logical_len as u64;
                }
            }
            ExternalStorageKind::BoxedArray => {
                // SAFETY: the allocation contains its length prefix.
                unsafe { *(published as *mut u64) = logical_len as u64 };
            }
        }
        ms.register_external_storage(published, base, layout, kind, logical_len);
        published
    }

    #[test]
    fn external_sweep_retains_marked_and_releases_unmarked_once() {
        let ms = MachineState::new();
        // SAFETY: helper registers allocations directly with this machine.
        let live = unsafe { register_test_external(&ms, ExternalStorageKind::Bytes, 3) };
        // SAFETY: helper registers allocations directly with this machine.
        let dead = unsafe { register_test_external(&ms, ExternalStorageKind::BoxedArray, 2) };
        let dead_slot = unsafe { dead.add(8) as *mut *mut u8 };
        ms.register_remembered_slot(dead_slot);

        let mut marked = HashSet::new();
        assert!(marked.insert(live));
        assert!(
            !marked.insert(live),
            "aliased payload identity is deduplicated"
        );
        let first = ms
            .commit_external_sweep(ms.plan_external_sweep(&marked).unwrap())
            .unwrap();
        assert_eq!(first.live_objects, 1);
        assert_eq!(first.freed_objects, 1);
        assert_eq!(ms.remembered_slots_count(), 0);

        let second = ms
            .commit_external_sweep(ms.plan_external_sweep(&marked).unwrap())
            .unwrap();
        assert_eq!(second.live_objects, 1);
        assert_eq!(second.freed_objects, 1);

        let final_stats = ms
            .commit_external_sweep(ms.plan_external_sweep(&HashSet::new()).unwrap())
            .unwrap();
        assert_eq!(final_stats.live_objects, 0);
        assert_eq!(final_stats.freed_objects, 2);
        assert_eq!(final_stats.freed_bytes, final_stats.allocated_bytes);
    }

    #[test]
    fn external_payload_view_validates_kind_length_and_slot_span() {
        let ms = MachineState::new();
        // SAFETY: helper registers the exact boxed-array layout.
        let boxed = unsafe { register_test_external(&ms, ExternalStorageKind::BoxedArray, 2) };
        let view = ms
            .external_payload_view(boxed, ExternalStorageKind::BoxedArray)
            .unwrap();
        assert_eq!(view.pointer_slots.len(), 2);
        assert_eq!(view.pointer_slots[0], unsafe {
            boxed.add(8) as *mut *mut u8
        });
        assert_eq!(view.pointer_slots[1], unsafe {
            boxed.add(16) as *mut *mut u8
        });
        assert!(matches!(
            ms.external_payload_view(boxed, ExternalStorageKind::Bytes),
            Err(ExternalStorageValidationError::KindMismatch { .. })
        ));
        assert!(matches!(
            ms.plan_external_sweep(&HashSet::from([boxed.wrapping_add(1)])),
            Err(ExternalStorageValidationError::Untracked(_))
        ));

        // SAFETY: the registered allocation contains its prefix; corrupt only
        // the logical value to prove validation occurs before slot traversal.
        unsafe { *(boxed as *mut u64) = 3 };
        let before = ms.external_storage_stats();
        assert!(matches!(
            ms.plan_external_sweep(&HashSet::new()),
            Err(ExternalStorageValidationError::LogicalLengthMismatch { .. })
        ));
        assert_eq!(ms.external_storage_stats(), before);

        // Align the ledger with the corrupted prefix so the registered
        // allocation is too short for its claimed pointer slots.
        unsafe { *(boxed as *mut u64) = 3 };
        ms.external_storage
            .borrow_mut()
            .get_mut(&boxed)
            .unwrap()
            .logical_len = 3;
        assert!(matches!(
            ms.plan_external_sweep(&HashSet::new()),
            Err(ExternalStorageValidationError::SpanExceedsAllocation { .. })
        ));
        assert_eq!(ms.external_storage_stats(), before);
    }

    #[test]
    fn external_sweep_rejects_stale_plan_before_mutation() {
        let ms = MachineState::new();
        // SAFETY: helper registers allocations directly with this machine.
        let first = unsafe { register_test_external(&ms, ExternalStorageKind::Bytes, 1) };
        let plan = ms.plan_external_sweep(&HashSet::new()).unwrap();
        // SAFETY: a second valid allocation changes the fenced ledger epoch.
        let _second = unsafe { register_test_external(&ms, ExternalStorageKind::Bytes, 2) };
        let before = ms.external_storage_stats();
        assert!(matches!(
            ms.commit_external_sweep(plan),
            Err(ExternalStorageValidationError::LedgerChanged)
        ));
        assert_eq!(ms.external_storage_stats(), before);
        assert!(ms.external_storage.borrow().contains_key(&first));
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
