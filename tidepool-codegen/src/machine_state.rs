//! Per-machine ambient state, reached either via `VMContext.machine_state`
//! (callers holding a `vmctx`) or via the per-thread [`CURRENT_MACHINE`] slot
//! (vmctx-less host fns and the external ambient shims).
//!
//! Homes all per-machine ambient state in one place, owned inline by each
//! `PreparedMachine`: the external-cancellation flag, the JSON decode
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
//! this permits hand-built test contexts that do not install a machine.
//!
//! External allocation is the narrow exception to the vmctx-only GC reach:
//! the existing byte/boxed allocation ABIs have no vmctx argument, so they
//! register through `CURRENT_MACHINE` before initializing or returning the
//! payload. `RegistryGuard` installs that pointer from the same
//! `PreparedMachine::machine_state` placed in vmctx, and restores it before
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
//! `PreparedMachine`) owns one directly, wires `vmctx.machine_state` at it,
//! and/or installs it as `CURRENT_MACHINE`, exercising the same reach paths
//! as production instead of a test-only backdoor.

use std::alloc::Layout;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::context::VMContext;
use crate::host_fns::{GcState, RuntimeError};
use crate::stack_map::{StackMapChain, StackMapIndex, StackMapRegistry};

pub use tidepool_heap::external_storage::{ExternalStorageKind, ExternalStorageValidationError};

/// All executable dispatch facts owned by one prepared descriptor.
///
/// A record is assembled before publication and thereafter only read. The
/// outer table is mutable solely at quiescent install and retirement points;
/// generated calls and enters therefore observe one coherent owner record
/// instead of consulting independently updated call and enter maps.
struct PreparedDispatchRecord {
    enter: Option<*const u8>,
    calls: std::collections::BTreeMap<tidepool_repr::execution_schema::Signature, *const u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PreparedCallContinuation {
    Return,
    Terminal,
    Apply,
}

pub(crate) struct PreparedCallResolution {
    pub code: *const u8,
    pub logical_consumed: usize,
    pub physical_consumed: usize,
    pub continuation: PreparedCallContinuation,
}

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

/// Whether a byte-range copy may alias its source and destination.
/// `copyByteArray#` requires distinct arrays; `copyMutableByteArray#` permits
/// one array with overlapping ranges (memmove).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ByteCopyAliasing {
    Disjoint,
    Overlapping,
}

/// The retained first cause and the reuse decision it imposed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineFailure {
    pub cause: RuntimeError,
    pub disposition: MachineDisposition,
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
    #[cfg(test)]
    pub(crate) fn into_major_slots(self) -> Vec<*mut *mut u8> {
        self.slots
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum ExternalGeneration {
    Young,
    Retained,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum ExternalActivity {
    Active,
    Revoked,
}

/// Allocation geometry for byte payloads. The published ABI stays
/// `[capacity][length][bytes]`; alignment padding precedes the capacity word.
/// The ledger must retain the true allocation base and this published offset.
fn aligned_byte_layout(
    logical_len: usize,
    alignment: usize,
) -> Result<(Layout, usize), ExternalStorageValidationError> {
    if !alignment.is_power_of_two() {
        return Err(ExternalStorageValidationError::LayoutAlignment { actual: alignment });
    }
    let alignment = alignment.max(8);
    let data_offset = alignment.max(16);
    let size = data_offset.checked_add(logical_len).ok_or(
        ExternalStorageValidationError::SpanOverflow {
            kind: ExternalStorageKind::Bytes,
            logical_len,
        },
    )?;
    let layout = Layout::from_size_align(size, alignment).map_err(|_| {
        ExternalStorageValidationError::SpanOverflow {
            kind: ExternalStorageKind::Bytes,
            logical_len,
        }
    })?;
    Ok((layout, data_offset - 8))
}

struct ExternalStorage {
    base: *mut u8,
    layout: Layout,
    published_offset: usize,
    #[allow(
        dead_code,
        reason = "consumed by the independently integrated major collector"
    )]
    kind: ExternalStorageKind,
    logical_len: usize,
    generation: ExternalGeneration,
    activity: ExternalActivity,
}

/// Validated pointer-bearing slots in one tracked external payload.
#[allow(
    dead_code,
    reason = "consumed by the independently integrated major collector"
)]
pub(crate) struct ExternalPayloadView {
    pub(crate) logical_len: usize,
    pub(crate) pointer_slots: tidepool_heap::external_storage::ExternalPointerSlots,
}

/// Allocation-bearing sweep plan produced before a major collector commits.
/// Fields remain private so callers cannot fabricate a partial dead set.
#[allow(
    dead_code,
    reason = "consumed by the independently integrated major collector"
)]
pub(crate) struct ExternalSweepPlan {
    revision: u64,
    allocated_objects: usize,
    live_objects: usize,
    dead: Vec<*mut u8>,
}

/// Per-machine ambient state. Each cell's wrapper type (`RefCell`/`Cell`) is
/// chosen to match the try_borrow/borrow-panic/take semantics its callers
/// rely on — see e.g. `set_first_cause`'s `try_borrow_mut` defense below.
pub struct MachineState {
    cancel_flag: RefCell<Option<Arc<AtomicBool>>>,
    #[cfg(test)]
    prepared_test_failure: RefCell<Option<PreparedTestFailure>>,
    stack_map_registry: RefCell<Vec<*const StackMapRegistry>>,
    /// Code-range index over `stack_map_registry`, populated exactly while
    /// two or more registries are linked (a single registry is searched
    /// directly). Every link/unlink path below keeps it in step with the
    /// chain; a frame walk takes an `Arc` snapshot so a walk never borrows
    /// the cell.
    stack_map_index: RefCell<Arc<StackMapIndex>>,
    runtime_error: RefCell<Option<RuntimeError>>,
    /// Prepared exception operand; independent of temporary observation marks.
    /// Prepared invocations keep this machine in Rc storage while snapshots use
    /// the slot's address. No heap-backed operand escapes in RuntimeError.
    prepared_exception: Cell<*mut u8>,
    /// A raised exception is being described; a raise inside the description
    /// is not described again.
    describing_exception: Cell<bool>,
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
    rust_roots: RefCell<Vec<(usize, *mut *mut u8)>>,
    next_rust_root: Cell<usize>,
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
    /// its arenas but hangs off `PreparedMachine`/`SessionState`, not
    /// reachable from `perform_gc` (vmctx -> `MachineState` only) — recording
    /// each arena's range here as it is allocated gives a diagnostic pass
    /// old-space bounds without threading `OldSpace` itself through vmctx.
    old_space_arenas: RefCell<Vec<(*const u8, *const u8)>>,
    /// Borrowed admission owner for prepared old-space arenas. The concrete
    /// pointer is installed only during a shared execution/observation borrow
    /// and cleared before promotion or owner movement; it is never a Send handle.
    prepared_old_space: RefCell<Option<*const crate::old_space::OldSpace>>,
    /// Program metadata available only while one synchronous prepared entry
    /// is executing. Rust-backed structural intrinsics use this to resolve
    /// authenticated constructors without a process registry. Nested installs
    /// are refused by the invocation owner.
    active_intrinsic_program: Cell<*const crate::prepared_program::CompiledProgram>,
    active_intrinsic_statics:
        Cell<*const std::sync::Arc<tidepool_heap::static_region::StaticRegion>>,
    active_intrinsic_statics_len: Cell<usize>,
    active_intrinsic_registry:
        Cell<*const std::collections::BTreeMap<usize, crate::prepared_program::DescriptorMetadata>>,
    /// Payloads allocated outside the moving heap. The map key is the pointer
    /// published in a Lit's value word; `base` may differ for byte arrays,
    /// whose ABI pointer follows a hidden allocation-size word.
    external_storage: RefCell<HashMap<*mut u8, ExternalStorage>>,
    /// None permanently invalidates sweep planning after revision exhaustion;
    /// an old plan must never become current again through integer wraparound.
    external_revision: Cell<Option<u64>>,
    external_allocated_bytes: Cell<usize>,
    external_allocated_objects: Cell<usize>,
    external_freed_bytes: Cell<usize>,
    external_freed_objects: Cell<usize>,
    /// Cross-program call and enter targets, keyed by descriptor header.
    /// Each immutable record is published as one unit after installation's
    /// fallible work and removed before its finalized code is released.
    prepared_dispatch: RefCell<HashMap<usize, PreparedDispatchRecord>>,
    /// Constructor descriptor header -> constructor identity, for every
    /// constructor an installed program declares. A prepared case miss reads
    /// its scrutinee's header here: a known constructor is an intact object
    /// the case did not expect (the call fails, the machine stays usable);
    /// anything else is an integrity failure.
    prepared_constructors: RefCell<HashMap<usize, tidepool_repr::DataConId>>,
    /// The one permanent, append-only literal-bytes pool for this machine:
    /// the sole authority for literal `Addr#` bytes, consulted by
    /// observation and by every address primitive before the ledger. Every
    /// installed program's compiled-in literal addresses resolve against it
    /// for the machine's whole life -- interned content is never removed,
    /// even once every program that referenced it has retired (see
    /// `intern_literal_bytes`/`absorb_interned_bytes`).
    interned_bytes: RefCell<Arc<crate::prepared_program::static_bytes::PinnedBytes>>,
}

// SAFETY: MachineState is only ever accessed from the single thread driving
// the owning PreparedMachine's run; the raw stack-map pointer is never
// dereferenced off that thread.
unsafe impl Send for MachineState {}

#[cfg(test)]
struct PreparedTestFailure {
    point: crate::prepared_control::PreparedSafepoint,
    remaining: usize,
    cause: RuntimeError,
}

// SAFETY: prepared collection/promotion borrows this machine for the complete
// no-mutator interval. Validation authenticates the live allocation and span;
// no external sweep, resize, or owner disposal runs during slot rewriting.
unsafe impl tidepool_heap::external_storage::ExternalPayloadOwner for MachineState {
    fn slots(
        &self,
        published: *mut u8,
        expected: ExternalStorageKind,
    ) -> Result<tidepool_heap::external_storage::ExternalPointerSlots, ExternalStorageValidationError>
    {
        self.external_payload_view(published, expected)
            .map(|view| view.pointer_slots)
    }
}

impl MachineState {
    pub fn new() -> Self {
        Self {
            cancel_flag: RefCell::new(None),
            #[cfg(test)]
            prepared_test_failure: RefCell::new(None),
            stack_map_registry: RefCell::new(Vec::new()),
            stack_map_index: RefCell::new(Arc::default()),
            runtime_error: RefCell::new(None),
            disposition: Cell::new(MachineDisposition::Reusable),
            last_failure: RefCell::new(None),
            diagnostics: RefCell::new(Vec::new()),
            gc_generation: Cell::new(0),
            gc_state: RefCell::new(None),
            rust_roots: RefCell::new(Vec::new()),
            next_rust_root: Cell::new(0),
            prepared_exception: Cell::new(std::ptr::null_mut()),
            describing_exception: Cell::new(false),
            persistent_roots: RefCell::new(Vec::new()),
            stowed_roots: RefCell::new(Vec::new()),
            code_roots: RefCell::new(HashSet::new()),
            write_barrier_armed: Cell::new(false),
            remembered_slots: RefCell::new(HashSet::new()),
            old_space_arenas: RefCell::new(Vec::new()),
            prepared_old_space: RefCell::new(None),
            active_intrinsic_program: Cell::new(std::ptr::null()),
            active_intrinsic_statics: Cell::new(std::ptr::null()),
            active_intrinsic_statics_len: Cell::new(0),
            active_intrinsic_registry: Cell::new(std::ptr::null()),
            external_storage: RefCell::new(HashMap::new()),
            external_revision: Cell::new(Some(0)),
            external_allocated_bytes: Cell::new(0),
            external_allocated_objects: Cell::new(0),
            external_freed_bytes: Cell::new(0),
            external_freed_objects: Cell::new(0),
            prepared_dispatch: RefCell::new(HashMap::new()),
            prepared_constructors: RefCell::new(HashMap::new()),
            interned_bytes: RefCell::new(Arc::new(
                crate::prepared_program::static_bytes::PinnedBytes::empty(),
            )),
        }
    }

    // --- stack map registry ---------------------------------------------
    // `pub`: bare-VMContext test harnesses (separate crates, no
    // PreparedMachine) install this directly on their own MachineState.

    /// Replace the whole chain with exactly this one registry. Single-program
    /// owners (the one-shot `PreparedInvocation`, and a `PreparedMachine`'s
    /// first installed program) use this; a later installed program on the
    /// same machine extends the chain with [`Self::push_stack_map_registry`]
    /// instead, so its frames are recognized without displacing an earlier
    /// program's registry.
    pub fn set_stack_map_registry(&self, registry: &StackMapRegistry) {
        let mut chain = self.stack_map_registry.borrow_mut();
        *chain = vec![registry as *const _];
        self.update_stack_map_index(&chain, |index| index.clear());
    }

    /// Extend the chain with one more registry, keeping every previously
    /// installed program's registry reachable. Return addresses never
    /// collide across pipelines, so the frame walker resolves each frame's
    /// address through the machine-wide code-range index.
    pub(crate) fn push_stack_map_registry(&self, registry: &StackMapRegistry) {
        let mut chain = self.stack_map_registry.borrow_mut();
        chain.push(registry as *const _);
        self.update_stack_map_index(&chain, |index| {
            // SAFETY: every linked registry is live while linked.
            unsafe {
                if chain.len() == 2 {
                    index.link(chain[0]);
                }
                index.link(registry);
            }
        });
    }

    /// Undo the most recent [`Self::push_stack_map_registry`]. Install-time
    /// rollback only: a failed second-or-later program install must not
    /// leave a dangling pointer into that program's (about to be dropped)
    /// pipeline in the chain -- unlike the owned descriptor/static-region
    /// unions, a raw stack-map pointer has no independent lifetime of its
    /// own, so this one entry cannot be left as merely "inert metadata".
    pub(crate) fn pop_stack_map_registry(&self) {
        let mut chain = self.stack_map_registry.borrow_mut();
        let Some(popped) = chain.pop() else {
            return;
        };
        self.update_stack_map_index(&chain, |index| {
            if chain.len() < 2 {
                index.clear();
            } else if !chain.iter().any(|&linked| std::ptr::eq(linked, popped)) {
                index.unlink(popped);
            }
        });
    }

    pub fn clear_stack_map_registry(&self) {
        let mut chain = self.stack_map_registry.borrow_mut();
        chain.clear();
        self.update_stack_map_index(&chain, |index| index.clear());
    }

    /// Unlink one program's registry by identity, wherever it sits in the
    /// chain: program retirement, which is not LIFO. `true` if it was linked.
    pub(crate) fn remove_stack_map_registry(&self, registry: &StackMapRegistry) -> bool {
        let mut chain = self.stack_map_registry.borrow_mut();
        let before = chain.len();
        chain.retain(|linked| !std::ptr::eq(*linked, registry));
        let removed = chain.len() != before;
        self.update_stack_map_index(&chain, |index| {
            if chain.len() < 2 {
                index.clear();
            } else if removed {
                index.unlink(registry);
            }
        });
        removed
    }

    /// Apply `edit` to the code-range index for the already-updated `chain`
    /// and check that the two agree. Copy-on-write: a snapshot still held by
    /// an abandoned walk is left untouched.
    fn update_stack_map_index(
        &self,
        chain: &[*const StackMapRegistry],
        edit: impl FnOnce(&mut StackMapIndex),
    ) {
        let mut index = self.stack_map_index.borrow_mut();
        if chain.len() < 2 && index.is_empty() {
            return;
        }
        let index = Arc::make_mut(&mut index);
        edit(index);
        debug_assert!(
            if chain.len() < 2 {
                index.is_empty()
            } else {
                // SAFETY: every linked registry is live while linked.
                unsafe { index.covers_exactly(chain) }
            },
            "stack-map index disagrees with the registry chain"
        );
    }

    /// The linked registries for one frame walk: `None` when nothing is
    /// linked. The snapshot must not outlive any registry linked now.
    pub(crate) fn stack_map_chain(&self) -> Option<StackMapChain> {
        let chain = self.stack_map_registry.borrow();
        match chain.as_slice() {
            [] => None,
            [single] => Some(StackMapChain::Single(*single)),
            _ => Some(StackMapChain::Indexed(Arc::clone(
                &self.stack_map_index.borrow(),
            ))),
        }
    }

    /// Number of linked stack-map registries: one per installed program
    /// (accounting for retirement receipts).
    pub(crate) fn stack_map_link_count(&self) -> usize {
        self.stack_map_registry.borrow().len()
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

    /// Every prepared poll records on this invocation. No TLS lookup is
    /// involved, including when a legacy machine is active on the same thread.
    pub(crate) fn poll_prepared(
        &self,
        _point: crate::prepared_control::PreparedSafepoint,
    ) -> crate::prepared_control::CallStatus {
        #[cfg(test)]
        {
            let mut pending = self.prepared_test_failure.borrow_mut();
            let fire = if let Some(failure) = pending.as_mut() {
                if failure.point == _point {
                    failure.remaining = failure.remaining.saturating_sub(1);
                    failure.remaining == 0
                } else {
                    false
                }
            } else {
                false
            };
            if fire {
                if let Some(failure) = pending.take() {
                    self.set_first_cause(failure.cause);
                }
            }
        }
        if self.cancel_requested() {
            self.set_first_cause(RuntimeError::Cancelled);
        }
        self.prepared_call_status()
    }

    /// Inject a first cause at a specific matching poll, without scheduling
    /// races or a production callback surface. Settlement uses the same status
    /// path as real cancellation, stack overflow and language failures.
    #[cfg(test)]
    pub(crate) fn fail_prepared_at(
        &self,
        point: crate::prepared_control::PreparedSafepoint,
        occurrence: usize,
        cause: RuntimeError,
    ) {
        *self.prepared_test_failure.borrow_mut() = Some(PreparedTestFailure {
            point,
            remaining: occurrence,
            cause,
        });
    }

    // --- Host-built value constructor ids ----------------------------------

    // --- runtime error (first-cause cell) -----------------------------------

    /// Record `cause` unless an earlier cause is already recorded — first
    /// write wins, because the earliest record is the one closest to the
    /// fault.
    ///
    /// Uses `try_borrow_mut` defensively so nested failure bookkeeping cannot
    /// panic while an earlier path still owns the cell. If the borrow fails,
    /// the earlier cause remains authoritative.
    pub(crate) fn set_first_cause(&self, cause: RuntimeError) {
        self.record_first_cause(cause, None);
    }

    /// # Safety
    /// A non-null operand is an admitted managed reference in this invocation.
    /// The machine must remain in its stable owner while collection uses roots.
    pub(crate) unsafe fn record_prepared_raise(&self, reference: *mut u8) {
        if self.disposition() == MachineDisposition::Unavailable {
            return;
        }
        if reference.is_null() {
            self.set_first_cause(RuntimeError::BadPointer);
        } else {
            self.record_first_cause(RuntimeError::RaisedException, Some(reference));
        }
    }

    /// Two slots, two lifetimes. `runtime_error` is the CALL OUTCOME: the
    /// first cause recorded during the current entry (language failure,
    /// cancellation, an unresolved cross-program callee, an integrity
    /// failure alike), consumed by the call's completion and reset by
    /// [`Self::begin_prepared_call`]. `last_failure` is the MACHINE LATCH:
    /// the first `Unavailable` cause ever recorded, never cleared, the
    /// reason every later entry and observation is refused. A reusable
    /// cause never reaches the latch, so an observation between two calls
    /// (`inspect_outer`) cannot read back a stale `Cancelled` or
    /// `UnresolvedCallee` from an unrelated realm's earlier call.
    fn record_first_cause(&self, cause: RuntimeError, exception: Option<*mut u8>) {
        let disposition = cause.machine_disposition();
        if disposition == MachineDisposition::Unavailable {
            self.disposition.set(MachineDisposition::Unavailable);
            if let Ok(mut latch) = self.last_failure.try_borrow_mut() {
                if latch.is_none() {
                    *latch = Some(MachineFailure {
                        cause: cause.clone(),
                        disposition,
                    });
                }
            }
        }
        if let Ok(mut slot) = self.runtime_error.try_borrow_mut() {
            if slot.is_none() {
                self.prepared_exception
                    .set(exception.unwrap_or(std::ptr::null_mut()));
                *slot = Some(cause);
            }
        }
    }

    /// Present the pending raise for description: clear the call's
    /// `RaisedException` so generated code may force its operand, and return
    /// the operand. The operand stays rooted until
    /// [`Self::restore_prepared_raise`], which the caller must call.
    pub(crate) fn suspend_prepared_raise(&self) -> Option<usize> {
        if self.disposition() != MachineDisposition::Reusable
            || self.describing_exception.get()
            || self.prepared_exception.get().is_null()
        {
            return None;
        }
        let mut slot = self.runtime_error.try_borrow_mut().ok()?;
        if !matches!(slot.as_ref(), Some(RuntimeError::RaisedException)) {
            return None;
        }
        *slot = None;
        self.describing_exception.set(true);
        Some(self.prepared_exception.get() as usize)
    }

    /// End a description begun by [`Self::suspend_prepared_raise`]. A failure
    /// the description itself recorded is dropped unless it latched the
    /// machine or was a cancellation; the call then fails with the raise,
    /// carrying the message when one was recovered.
    pub(crate) fn restore_prepared_raise(&self, message: Option<String>) {
        self.describing_exception.set(false);
        if self.disposition() == MachineDisposition::Unavailable {
            return;
        }
        let exception = self.prepared_exception.get();
        if let Ok(mut slot) = self.runtime_error.try_borrow_mut() {
            if matches!(slot.as_ref(), Some(RuntimeError::Cancelled)) {
                return;
            }
            *slot = None;
        }
        let cause = message.map_or(
            RuntimeError::RaisedException,
            RuntimeError::RaisedExceptionMessage,
        );
        self.record_first_cause(cause, (!exception.is_null()).then_some(exception));
    }

    pub(crate) fn disposition(&self) -> MachineDisposition {
        self.disposition.get()
    }

    /// The machine latch: the first `Unavailable` cause, if any. `None` on a
    /// reusable machine even while a call outcome is pending.
    pub(crate) fn last_failure(&self) -> Option<MachineFailure> {
        self.last_failure.try_borrow().ok().and_then(|f| f.clone())
    }

    /// The failure a completing call reports: the machine latch when set,
    /// otherwise the pending call outcome (reusable by construction).
    pub(crate) fn current_failure(&self) -> Option<MachineFailure> {
        if let Some(latched) = self.last_failure() {
            return Some(latched);
        }
        self.runtime_error
            .try_borrow()
            .ok()
            .and_then(|slot| slot.clone())
            .map(|cause| MachineFailure {
                disposition: cause.machine_disposition(),
                cause,
            })
    }

    /// Start a fresh prepared entry on this machine.
    ///
    /// Language failure and cancellation settle one entry only.  An integrity
    /// failure is a machine property and therefore cannot be cleared here.
    /// Callers must have removed their run-scoped roots before beginning the
    /// next entry; persistent roots intentionally remain installed.
    pub(crate) fn begin_prepared_call(&self) -> Result<(), MachineFailure> {
        if self.disposition() == MachineDisposition::Unavailable {
            return Err(self.last_failure().unwrap_or(MachineFailure {
                cause: RuntimeError::BadPointer,
                disposition: MachineDisposition::Unavailable,
            }));
        }
        self.prepared_exception.set(std::ptr::null_mut());
        self.runtime_error.borrow_mut().take();
        // `last_failure` is the Unavailable latch and is never cleared; a
        // reusable machine has none to clear.
        Ok(())
    }

    /// Settle the entry that just returned. Its outcome has been reported by
    /// the returning call (`current_failure`); consuming it here means no
    /// later observation, collection or root write between calls can mistake
    /// that call's reusable outcome for a live fault (`prepared_call_status`
    /// gates those paths). The Unavailable latch is untouched.
    pub(crate) fn end_prepared_call(&self) {
        let _ = self.take_runtime_error();
    }

    /// Take the pending cause, if any. Uses `try_borrow_mut` defensively so
    /// cleanup does not panic if nested failure bookkeeping still owns the
    /// cell.
    /// Consuming a prepared cause settles/releases its exception operand.
    /// Any future operand presentation must precede this operation.
    pub(crate) fn take_runtime_error(&self) -> Option<RuntimeError> {
        let cause = self
            .runtime_error
            .try_borrow_mut()
            .ok()
            .and_then(|mut e| e.take());
        if cause.is_some() {
            self.prepared_exception.set(std::ptr::null_mut());
        }
        cause
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

    // --- Prepared nursery state (leaf 3) ----------------------------------

    /// Install a retained session heap buffer as the active GC region.
    /// Install one prepared heap with its pinned compiled-layout owners.
    /// A live heap must be retired by its owning run before another is installed.
    #[cfg(test)]
    pub(crate) fn install_prepared_buffer(
        &self,
        buffer: Vec<u64>,
        layouts: Vec<std::sync::Arc<tidepool_heap::execution_descriptor::ObjectDescriptor>>,
    ) -> Result<(), RuntimeError> {
        self.install_prepared_buffer_with_static_region(buffer, layouts, None)
    }

    /// Read the authoritative, zero-based family tag after generated Enter.
    /// Pointer low bits are evidence, not the constructor's identity.
    ///
    /// # Safety
    /// A non-null untagged address must point to a readable initialized object header
    /// whose storage remains owned by this invocation. This is not admission of
    /// arbitrary host pointers; generated code establishes reference provenance.
    pub(crate) unsafe fn prepared_constructor_tag(
        &self,
        encoded: usize,
    ) -> Result<i64, RuntimeError> {
        use tidepool_heap::execution_descriptor::{DescriptorState, ObjectKind};
        use tidepool_heap::managed_reference::{tag_valid, untag};
        let reference = untag(encoded) as *const usize;
        if reference.is_null() {
            return Err(RuntimeError::BadPointer);
        }
        let active = self
            .gc_state
            .try_borrow()
            .map_err(|_| RuntimeError::BadPointer)?;
        let prepared = active
            .as_ref()
            .and_then(|state| state.prepared.as_ref())
            .ok_or(RuntimeError::BadPointer)?;
        let header = unsafe { reference.read() };
        if header & 7 != 0 {
            return Err(RuntimeError::BadThunkState((header & 7) as u8));
        }
        let descriptor = prepared
            .space
            .live_descriptor(header)
            .ok_or(RuntimeError::BadPointer)?;
        if descriptor.kind() != ObjectKind::Constructor {
            return Err(RuntimeError::ExpectedConstructor);
        }
        let tag = descriptor
            .constructor_tag()
            .ok_or(RuntimeError::ExpectedConstructor)?;
        if !tag_valid(
            (encoded & 7) as u8,
            descriptor.kind(),
            DescriptorState::Live,
            Some(tag),
        ) {
            return Err(RuntimeError::BadPointer);
        }
        Ok(i64::from(tag.get() - 1))
    }

    /// Resolve the physical kind of a live prepared object through the active
    /// descriptor space. This lets generic enter recognize evaluated values
    /// without embedding every installed function, PAP, and constructor
    /// header in generated code.
    ///
    /// # Safety
    /// `encoded` has the same managed-reference provenance requirement as
    /// [`Self::prepared_constructor_tag`].
    pub(crate) unsafe fn prepared_object_kind(
        &self,
        encoded: usize,
    ) -> Result<tidepool_heap::execution_descriptor::ObjectKind, RuntimeError> {
        use tidepool_heap::managed_reference::untag;
        let reference = untag(encoded) as *const usize;
        if reference.is_null() {
            return Err(RuntimeError::BadPointer);
        }
        let active = self
            .gc_state
            .try_borrow()
            .map_err(|_| RuntimeError::BadPointer)?;
        let prepared = active
            .as_ref()
            .and_then(|state| state.prepared.as_ref())
            .ok_or(RuntimeError::BadPointer)?;
        let header = unsafe { reference.read() };
        if header & 7 != 0 {
            return Err(RuntimeError::BadThunkState((header & 7) as u8));
        }
        prepared
            .space
            .live_descriptor(header)
            .map(|descriptor| descriptor.kind())
            .ok_or(RuntimeError::BadPointer)
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
        // `layouts` is not read again after this call: hand its Arcs to the
        // space by move (`into_iter`), not by a second Arc-bump pass over a
        // borrow -- the caller already paid for one clone to get an owned
        // `Vec` here (see `PreparedMachine::compile_for_install`'s callers).
        let space = tidepool_heap::gc::raw::DescriptorSpace::new(layouts)
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

    /// Union one more installed program's pinned descriptor layouts and
    /// immutable static image into the already-active prepared descriptor
    /// space, mutating it in place. Unlike
    /// [`Self::install_prepared_buffer_with_static_region`] (which requires
    /// no active `GcState`), this requires one already installed -- it is
    /// the second-and-later-program path on a `PreparedMachine` that shares
    /// one nursery/`GcState` across every installed program.
    pub(crate) fn extend_prepared_descriptors(
        &self,
        layouts: Vec<std::sync::Arc<tidepool_heap::execution_descriptor::ObjectDescriptor>>,
        static_region: Arc<tidepool_heap::static_region::StaticRegion>,
    ) -> Result<(), RuntimeError> {
        let mut active = self
            .gc_state
            .try_borrow_mut()
            .map_err(|_| RuntimeError::BadPointer)?;
        let prepared = active
            .as_mut()
            .and_then(|state| state.prepared.as_mut())
            .ok_or(RuntimeError::BadPointer)?;
        prepared
            .space
            .extend_descriptors(layouts)
            .map_err(|_| RuntimeError::HeapOverflow)?;
        prepared
            .space
            .extend_static_region(static_region)
            .map_err(|_| RuntimeError::HeapOverflow)?;
        Ok(())
    }

    /// Open the descriptor space's registration undo log (see
    /// [`tidepool_heap::gc::raw::OwnersMark`]); `None` when no prepared heap
    /// exists yet.
    pub(crate) fn mark_prepared_descriptor_owners(
        &self,
    ) -> Option<tidepool_heap::gc::raw::OwnersMark> {
        self.gc_state
            .borrow_mut()
            .as_mut()?
            .prepared
            .as_mut()
            .map(|prepared| prepared.space.mark_owners())
    }

    /// Close `mark`, keeping (`commit`) or removing everything registered
    /// since it was opened.
    pub(crate) fn finish_prepared_descriptor_owners(
        &self,
        mark: tidepool_heap::gc::raw::OwnersMark,
        commit: bool,
    ) {
        let mut active = self.gc_state.borrow_mut();
        // A failed install may already have torn the space down; there is
        // then nothing left to undo.
        let Some(prepared) = active.as_mut().and_then(|state| state.prepared.as_mut()) else {
            return;
        };
        if commit {
            prepared.space.commit_owners(mark);
        } else {
            prepared.space.rollback_owners(mark);
        }
    }

    /// Reclaim the live heap buffer + high-water cursor from this machine's
    /// GC state, called from `RegistryGuard::drop` BEFORE `clear_run_scratch`
    /// takes the `GcState`. Returns `(None, 0)` when there's no `GcState`
    /// installed (e.g. a run that never reached GC setup).
    #[cfg(test)]
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
        self.prepared_exception.set(std::ptr::null_mut());
        self.gc_state.borrow_mut().take();
        self.clear_rust_roots();
    }

    /// PER-RUN teardown: take `GcState` (the `active_buffer` was already
    /// reclaimed by `reclaim_session_heap` before this runs) and clear the
    /// per-run rust roots. Does NOT touch `persistent_roots` — those are
    /// session-scoped and survive until `free_session_heap`.
    #[cfg(test)]
    pub(crate) fn clear_run_scratch(&self) {
        self.prepared_exception.set(std::ptr::null_mut());
        self.gc_state.borrow_mut().take();
        self.clear_rust_roots();
    }

    /// MACHINE-DROP teardown: clear session-scoped persistent roots and take
    /// `GcState`. Called by `PreparedMachine::drop`. Operates directly on
    /// `self` (not through any ambient reach) so it always clears exactly
    /// the dying machine's own registries.
    pub(crate) fn free_session_heap(&self) {
        self.prepared_exception.set(std::ptr::null_mut());
        self.clear_persistent_roots();
        // Defensive: a machine dropped mid-nested-child (a child panicked and
        // its guard unwound) must not leave a dangling stowed slot registered.
        self.clear_stowed_roots();
        // Per-arena `retire_old_space_arena` calls (PreparedMachine::drop,
        // before this runs) already forget slots pointing into old-space; this
        // is the blanket net for anything left (e.g. a boxed-array payload
        // slot, which lives in an external malloc'd buffer outside every
        // arena range).
        self.clear_remembered_slots();
        self.gc_state.borrow_mut().take();
    }

    /// Take this machine's `GcState` out of its cell, leaving the cell empty.
    /// `perform_gc` uses this to operate on an owned `GcState` across the
    /// Cheney copy without holding a `RefCell` borrow through collection.
    /// Pair with [`Self::put_gc_state`].
    pub(crate) fn take_gc_state(&self) -> Option<GcState> {
        self.gc_state.borrow_mut().take()
    }

    /// Put a `GcState` previously removed by [`Self::take_gc_state`] back
    /// into the cell.
    pub(crate) fn put_gc_state(&self, state: GcState) {
        *self.gc_state.borrow_mut() = Some(state);
    }

    // --- rust roots (run-scoped GC roots, leaf 3) --------------------------

    pub(crate) fn register_rust_root(&self, slot: *mut *mut u8) -> usize {
        let registration = self.next_rust_root.get();
        self.next_rust_root.set(
            registration
                .checked_add(1)
                .expect("temporary root registration space exhausted"),
        );
        let mut roots = self.rust_roots.borrow_mut();
        roots.push((registration, slot));
        registration
    }

    /// Stop tracing one temporary Rust root without moving registrations owned
    /// by nested operations across their stack marks.
    pub(crate) fn deregister_rust_root(&self, registration: usize, slot: *mut *mut u8) {
        let mut roots = self.rust_roots.borrow_mut();
        if let Some(index) = roots
            .iter()
            .position(|candidate| *candidate == (registration, slot))
        {
            roots.remove(index);
        }
    }

    /// Remove a sorted set of exact registrations in one linear pass. This is
    /// used when a construction owner drops with many reusable DAG roots.
    pub(crate) fn deregister_rust_roots(&self, registrations: &[usize]) {
        self.rust_roots
            .borrow_mut()
            .retain(|(registration, _)| registrations.binary_search(registration).is_err());
    }

    /// Number of active temporary roots; excludes the independent exception.
    pub(crate) fn rust_roots_len(&self) -> usize {
        self.rust_roots.borrow().len()
    }

    pub(crate) fn rust_roots_mark(&self) -> usize {
        self.next_rust_root.get()
    }

    pub(crate) fn truncate_rust_roots(&self, mark: usize) {
        self.rust_roots
            .borrow_mut()
            .retain(|(registration, _)| *registration < mark);
    }

    /// Clear temporary registrations, not the exception settlement slot.
    pub(crate) fn clear_rust_roots(&self) {
        self.rust_roots.borrow_mut().clear();
    }

    /// Append this machine's run-scoped rust roots to `out` — used by
    /// `perform_gc` to build its root slot list.
    pub(crate) fn extend_rust_roots(&self, out: &mut Vec<*mut *mut u8>) {
        out.extend(self.rust_roots.borrow().iter().map(|(_, slot)| *slot));
        if !self.prepared_exception.get().is_null() {
            out.push(self.prepared_exception.as_ptr());
        }
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
    /// (`PreparedMachine::close_realm`): a released value's slot cell stays
    /// allocated (owned by `OldSpace::slots` for the machine's life — 8 bytes),
    /// but the GC stops tracing and rewriting it, so the value it pinned can
    /// be collected once nothing else reaches it.
    pub(crate) fn deregister_persistent_root(&self, slot: *mut *mut u8) {
        let mut roots = self.persistent_roots.borrow_mut();
        if let Some(pos) = roots.iter().position(|&s| s == slot) {
            roots.remove(pos);
        }
    }

    /// Deregister a whole batch of persistent-root slots (e.g. a program's
    /// entire root block) in one pass instead of one linear search +
    /// `Vec::remove` per slot — O(slots + roots) instead of O(slots * roots).
    /// Matches [`Self::deregister_persistent_root`] called once per address in
    /// `roots`, including its multiplicity: a slot registered N times and
    /// listed N times in `roots` has all N registrations removed; listed
    /// fewer than N times, only that many are removed (earliest occurrences
    /// first, same as repeated single calls, which always remove the first
    /// remaining match). An address in `roots` that was never registered, or
    /// listed more times than it was registered, is a no-op for the surplus —
    /// idempotent like the single-root version. `HashSet` alone can't carry
    /// per-address multiplicity, so this counts remaining removals per
    /// address instead.
    pub(crate) fn deregister_persistent_roots(&self, roots: &[*mut *mut u8]) {
        if roots.is_empty() {
            return;
        }
        let mut remaining: HashMap<*mut *mut u8, usize> = HashMap::with_capacity(roots.len());
        for &slot in roots {
            *remaining.entry(slot).or_insert(0) += 1;
        }
        self.persistent_roots.borrow_mut().retain(|slot| {
            if let Some(count) = remaining.get_mut(slot) {
                if *count > 0 {
                    *count -= 1;
                    return false;
                }
            }
            true
        });
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

    #[cfg(test)]
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

    /// Record `slot` in the remembered set. The one sink for every
    /// old-to-young edge recorder (`host_fns::write_barrier`,
    /// `retain_external_payloads`); the test-only kill switch lives here so a
    /// mutation check disables recording as a whole, not one recorder.
    pub(crate) fn register_remembered_slot(&self, slot: *mut *mut u8) {
        if crate::host_fns::remembered_set_disabled_for_test() {
            return;
        }
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

    // --- cross-program call/enter resolution ------------------------------
    // Substrate for the prepared engine's cross-program call and force
    // fallback (see `prepared_program::resolve`); `apply::emit_dispatchers`'
    // fallback and `entry::emit_prepared_enter` read the same owner record.

    /// Register one installed program's exported call targets and owned
    /// enter headers. Headers are unique per descriptor across the machine;
    /// each program emits one target per compatible header/signature pair.
    pub(crate) fn register_prepared_entries(
        &self,
        callables: impl IntoIterator<
            Item = (usize, tidepool_repr::execution_schema::Signature, *const u8),
        >,
        enters: impl IntoIterator<Item = (usize, *const u8)>,
    ) {
        let mut staged = HashMap::<usize, PreparedDispatchRecord>::new();
        for (header, signature, code) in callables {
            staged
                .entry(header)
                .or_insert_with(|| PreparedDispatchRecord {
                    enter: None,
                    calls: std::collections::BTreeMap::new(),
                })
                .calls
                .insert(signature, code);
        }
        for (header, enter) in enters {
            staged
                .entry(header)
                .or_insert_with(|| PreparedDispatchRecord {
                    enter: None,
                    calls: std::collections::BTreeMap::new(),
                })
                .enter = Some(enter);
        }
        self.prepared_dispatch.borrow_mut().extend(staged);
    }

    /// Record constructor headers an installed program declares; see
    /// `prepared_constructors`. Interned constructor headers are shared, so a
    /// repeated header keeps its identity.
    pub(crate) fn register_prepared_constructors(
        &self,
        constructors: impl IntoIterator<Item = (usize, tidepool_repr::DataConId)>,
    ) {
        let mut known = self.prepared_constructors.borrow_mut();
        for (header, identity) in constructors {
            known.entry(header).or_insert(identity);
        }
    }

    /// Forget constructor headers a retired program owned.
    pub(crate) fn retire_prepared_constructors(&self, headers: &[usize]) {
        let mut known = self.prepared_constructors.borrow_mut();
        for header in headers {
            known.remove(header);
        }
    }

    /// Classify a prepared case miss by its scrutinee's header word.
    pub(crate) fn prepared_constructor_at(
        &self,
        header: usize,
    ) -> Option<tidepool_repr::DataConId> {
        self.prepared_constructors
            .try_borrow()
            .ok()?
            .get(&header)
            .copied()
    }

    /// Machine-teardown path (`Drop for PreparedMachine`): drop every raw
    /// code pointer before the pipelines they point into are freed.
    pub(crate) fn clear_prepared_entries(&self) {
        self.prepared_dispatch.borrow_mut().clear();
        self.prepared_constructors.borrow_mut().clear();
    }

    /// Program retirement: drop the call and enter rows one program owned,
    /// before its pipeline is freed. An enterable header is owned by exactly
    /// one program (its descriptors are minted per compile), so a retired
    /// owner's rows have no other pointer to switch to; a header a live object
    /// still carried would have kept the program live.
    pub(crate) fn retire_prepared_entries(&self, headers: &[usize]) {
        let mut records = self.prepared_dispatch.borrow_mut();
        for header in headers {
            records.remove(header);
        }
    }

    /// `(callable rows, enter rows)` currently registered.
    pub(crate) fn prepared_entry_rows(&self) -> (usize, usize) {
        let records = self.prepared_dispatch.borrow();
        let callable_rows = records
            .values()
            .filter(|record| !record.calls.is_empty())
            .count();
        let enter_rows = records
            .values()
            .filter(|record| record.enter.is_some())
            .count();
        (callable_rows, enter_rows)
    }

    /// Program retirement's precondition: the live descriptor space is
    /// installed and not borrowed, so [`Self::retire_prepared_descriptors`]
    /// cannot miss it. Checked once before the first descriptor leaves.
    pub(crate) fn check_prepared_descriptor_space(&self) -> Result<(), RuntimeError> {
        let active = self
            .gc_state
            .try_borrow()
            .map_err(|_| RuntimeError::BadPointer)?;
        active
            .as_ref()
            .and_then(|state| state.prepared.as_ref())
            .map(|_| ())
            .ok_or(RuntimeError::BadPointer)
    }

    /// Program retirement: remove the layouts only that program owned and
    /// its static region from the live descriptor space. The caller checked
    /// [`Self::check_prepared_descriptor_space`] and nothing between that
    /// check and this call takes the GC state, so the space is always found.
    /// Were it not, keeping the layouts admitted is the safe outcome: a
    /// descriptor that outlives its program is a leak, never a failure.
    pub(crate) fn retire_prepared_descriptors(
        &self,
        headers: &[usize],
        region: &Arc<tidepool_heap::static_region::StaticRegion>,
    ) {
        let Ok(mut active) = self.gc_state.try_borrow_mut() else {
            debug_assert!(false, "retirement checked the descriptor space");
            return;
        };
        let Some(prepared) = active.as_mut().and_then(|state| state.prepared.as_mut()) else {
            debug_assert!(false, "retirement checked the descriptor space");
            return;
        };
        prepared.space.retire_owner(headers, Some(region));
    }

    pub(crate) fn resolve_prepared_application(
        &self,
        header: usize,
        demand: &tidepool_repr::execution_schema::Signature,
        logical_cursor: usize,
    ) -> Option<PreparedCallResolution> {
        use tidepool_repr::execution_schema::{ResultContract, RuntimeRep};

        let remaining = demand.arguments.get(logical_cursor..)?;
        let records = self.prepared_dispatch.borrow();
        let calls = &records.get(&header)?.calls;
        let physical = |logical: &[RuntimeRep]| {
            logical
                .iter()
                .filter(|rep| **rep != RuntimeRep::Void)
                .count()
        };
        let resolution = |code, consumed, continuation| PreparedCallResolution {
            code,
            logical_consumed: consumed,
            physical_consumed: physical(&remaining[..consumed]),
            continuation,
        };

        if let Some((signature, &code)) = calls.iter().find(|(signature, _)| {
            signature.arguments.as_slice() == remaining && signature.results == demand.results
        }) {
            return Some(resolution(
                code,
                signature.arguments.len(),
                PreparedCallContinuation::Return,
            ));
        }

        let mut terminal = None;
        let mut apply = None;
        for (signature, &code) in calls {
            let consumed = signature.arguments.len();
            if consumed > remaining.len()
                || signature.arguments.as_slice() != &remaining[..consumed]
            {
                continue;
            }
            match &signature.results {
                ResultContract::NoSuccess => {
                    if terminal
                        .as_ref()
                        .is_none_or(|(_, previous)| consumed > *previous)
                    {
                        terminal = Some((code, consumed));
                    }
                }
                ResultContract::Returns(reps)
                    if demand.results != ResultContract::NoSuccess
                        && reps.as_slice() == [RuntimeRep::LiftedRef]
                        && consumed > 0
                        && consumed < remaining.len()
                        && apply
                            .as_ref()
                            .is_none_or(|(_, previous)| consumed > *previous) =>
                {
                    apply = Some((code, consumed));
                }
                ResultContract::Returns(_) | ResultContract::CallerResult => {}
            }
        }
        terminal
            .map(|(code, consumed)| resolution(code, consumed, PreparedCallContinuation::Terminal))
            .or_else(|| {
                apply.map(|(code, consumed)| {
                    resolution(code, consumed, PreparedCallContinuation::Apply)
                })
            })
    }

    pub(crate) fn resolve_prepared_enter(&self, header: usize) -> Option<*const u8> {
        self.prepared_dispatch.borrow().get(&header)?.enter
    }

    /// Whether some installed program owns an enter routine for `header` (a
    /// masked object header word): the object is a real function, PAP or
    /// thunk of this machine. A call-resolution miss on such a header is an
    /// ordinary, reusable `UnresolvedCallee` (incompatible demand); a miss
    /// on any other header means the callee word does not name a callable
    /// object at all, which is an integrity failure.
    pub(crate) fn owns_prepared_entry(&self, header: usize) -> bool {
        self.prepared_dispatch.borrow().contains_key(&header)
    }

    /// Join a successful generated-frame walk with every ambient root registry.
    ///
    /// This method does not make an unsuccessful frame walk complete. Its caller
    /// must construct the stack-root slice only after `walk_frames` succeeds;
    /// the opaque return type then prevents downstream collectors from rebuilding
    /// a partial registry list.
    pub(crate) unsafe fn complete_root_snapshot(
        &self,
        stack_roots: &[*mut *mut u8],
    ) -> GcRootSnapshot {
        let mut slots = Vec::with_capacity(
            stack_roots.len()
                + self.rust_roots.borrow().len()
                + self.persistent_roots.borrow().len()
                + self.stowed_roots.borrow().len()
                + self.code_roots.borrow().len()
                + self.remembered_slots.borrow().len()
                + usize::from(!self.prepared_exception.get().is_null()),
        );
        slots.extend_from_slice(stack_roots);
        self.extend_rust_roots(&mut slots);
        self.extend_persistent_roots(&mut slots);
        self.extend_stowed_roots(&mut slots);
        self.extend_code_roots(&mut slots);
        let mut remembered_slots = Vec::with_capacity(self.remembered_slots.borrow().len());
        self.extend_remembered_slots(&mut remembered_slots);

        GcRootSnapshot {
            slots,
            remembered_slots,
        }
    }

    /// Snapshot of every currently-remembered slot. Read-only; does not
    /// affect GC. Read by `host_fns::gc`'s post-GC `verify_remembered_slots`
    /// pass under `TIDEPOOL_HEAP_VERIFY`.
    #[cfg(test)]
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

    /// Install the invocation-owned prepared old-space admission view.
    ///
    /// # Safety
    /// `owner` must remain allocated and immutable for every collection or
    /// observation that can reach this machine until
    /// [`Self::clear_prepared_old_space`] is called.
    pub(crate) unsafe fn install_prepared_old_space(&self, owner: &crate::old_space::OldSpace) {
        *self.prepared_old_space.borrow_mut() = Some(owner as *const _);
    }

    /// Clear the borrowed prepared admission pointer before its owner drops.
    pub(crate) fn clear_prepared_old_space(&self) {
        *self.prepared_old_space.borrow_mut() = None;
    }

    pub(crate) fn install_active_intrinsic_program(
        &self,
        program: &crate::prepared_program::CompiledProgram,
        statics: &[std::sync::Arc<tidepool_heap::static_region::StaticRegion>],
        registry: &std::collections::BTreeMap<usize, crate::prepared_program::DescriptorMetadata>,
    ) -> bool {
        if !self.active_intrinsic_program.get().is_null() {
            return false;
        }
        self.active_intrinsic_program.set(program);
        self.active_intrinsic_statics.set(statics.as_ptr());
        self.active_intrinsic_statics_len.set(statics.len());
        self.active_intrinsic_registry.set(registry);
        true
    }

    pub(crate) fn clear_active_intrinsic_program(&self) {
        self.active_intrinsic_program.set(std::ptr::null());
        self.active_intrinsic_statics.set(std::ptr::null());
        self.active_intrinsic_statics_len.set(0);
        self.active_intrinsic_registry.set(std::ptr::null());
    }

    /// # Safety
    /// The returned borrow is bounded by the synchronous invocation scope
    /// that installed the stable compiled-program owner.
    #[allow(dead_code, reason = "used by the prepared JSON intrinsic sink")]
    pub(crate) unsafe fn active_intrinsic_program(
        &self,
    ) -> Option<&crate::prepared_program::CompiledProgram> {
        self.active_intrinsic_program.get().as_ref()
    }

    /// # Safety
    /// The returned borrows are bounded by the synchronous intrinsic scope.
    pub(crate) unsafe fn active_intrinsic_observation(
        &self,
    ) -> Option<(
        &[std::sync::Arc<tidepool_heap::static_region::StaticRegion>],
        &std::collections::BTreeMap<usize, crate::prepared_program::DescriptorMetadata>,
    )> {
        let statics = self.active_intrinsic_statics.get();
        let registry = self.active_intrinsic_registry.get();
        if statics.is_null() || registry.is_null() {
            return None;
        }
        Some((
            unsafe { std::slice::from_raw_parts(statics, self.active_intrinsic_statics_len.get()) },
            unsafe { &*registry },
        ))
    }

    /// Borrow the exact-start admission owner for one collector/observer call.
    ///
    /// # Safety
    /// The caller must uphold the owner lifetime established by
    /// [`Self::install_prepared_old_space`].
    pub(crate) unsafe fn prepared_old_space(&self) -> Option<&crate::old_space::OldSpace> {
        self.prepared_old_space
            .borrow()
            .as_ref()
            .map(|&pointer| &*pointer)
    }

    /// Check the immutable static owner of the active prepared descriptor
    /// space. This is used by retention promotion's owner gate before it
    /// treats an outside-nursery result as already stable.
    pub(crate) fn prepared_static_reference(
        &self,
        encoded: usize,
    ) -> Result<Option<usize>, tidepool_heap::execution_descriptor::DescriptorTraceError> {
        let state = self
            .gc_state
            .try_borrow()
            .map_err(|_| tidepool_heap::execution_descriptor::DescriptorTraceError::InvalidRange)?;
        let prepared = state
            .as_ref()
            .and_then(|state| state.prepared.as_ref())
            .ok_or(tidepool_heap::execution_descriptor::DescriptorTraceError::InvalidRange)?;
        prepared.space.admit_static_reference(encoded)
    }

    // --- GC-external byte/reference storage -------------------------------

    fn external_changed(&self) {
        self.external_revision.set(
            self.external_revision
                .get()
                .and_then(|revision| revision.checked_add(1)),
        );
    }

    fn validate_external_access(
        published: *mut u8,
        record: &ExternalStorage,
        expected: ExternalStorageKind,
        active_only: bool,
    ) -> Result<(), ExternalStorageValidationError> {
        if active_only && record.activity == ExternalActivity::Revoked {
            return Err(ExternalStorageValidationError::Revoked(published as usize));
        }
        if record.kind != expected {
            return Err(ExternalStorageValidationError::KindMismatch {
                expected,
                actual: record.kind,
            });
        }
        Self::validate_external_record(published, record)
    }

    fn structural_external_record(
        storage: &HashMap<*mut u8, ExternalStorage>,
        published: *mut u8,
        expected: ExternalStorageKind,
    ) -> Result<&ExternalStorage, ExternalStorageValidationError> {
        let record = storage
            .get(&published)
            .ok_or(ExternalStorageValidationError::Untracked(
                published as usize,
            ))?;
        Self::validate_external_access(published, record, expected, false)?;
        Ok(record)
    }

    fn checked_external_record(
        storage: &HashMap<*mut u8, ExternalStorage>,
        published: *mut u8,
        expected: ExternalStorageKind,
    ) -> Result<&ExternalStorage, ExternalStorageValidationError> {
        let record = Self::structural_external_record(storage, published, expected)?;
        if record.activity == ExternalActivity::Revoked {
            return Err(ExternalStorageValidationError::Revoked(published as usize));
        }
        Ok(record)
    }

    /// Store an entire checked range without a safepoint between validation,
    /// remembered-set admission, and writes. Young slots are not roots;
    /// Retained slots are remembered before the values become visible.
    pub(crate) fn store_external_elements(
        &self,
        published: *mut u8,
        start: usize,
        values: &[*mut u8],
    ) -> Result<(), ExternalStorageValidationError> {
        let storage = self.external_storage.borrow();
        let record =
            Self::checked_external_record(&storage, published, ExternalStorageKind::BoxedArray)?;
        let end = start.checked_add(values.len()).ok_or(
            ExternalStorageValidationError::IndexOutOfBounds {
                index: start,
                len: record.logical_len,
            },
        )?;
        if end > record.logical_len {
            return Err(ExternalStorageValidationError::IndexOutOfBounds {
                index: end - 1,
                len: record.logical_len,
            });
        }
        // SAFETY: validation and the checked range place every slot in the allocation.
        let first = unsafe { published.add(8).cast::<*mut u8>().add(start) };
        if record.generation == ExternalGeneration::Retained {
            let mut remembered = self.remembered_slots.borrow_mut();
            remembered
                .try_reserve(values.len())
                .map_err(|_| ExternalStorageValidationError::BookkeepingAllocation)?;
            for index in 0..values.len() {
                remembered.insert(unsafe { first.add(index) });
            }
        }
        for (index, &value) in values.iter().enumerate() {
            unsafe { first.add(index).write(value) };
        }
        if !values.is_empty() {
            self.external_changed();
        }
        Ok(())
    }

    /// Copy a mutable boxed-array range with memmove semantics. Authenticate both
    /// complete spans before allocation or mutation, snapshot the source, then
    /// use the existing bulk store owner for retained-slot barriers and revision.
    /// No safepoint or callback may occur while the pointer snapshot is live.
    pub(crate) fn copy_external_elements(
        &self,
        source: *mut u8,
        source_start: usize,
        destination: *mut u8,
        destination_start: usize,
        count: usize,
    ) -> Result<(), ExternalStorageValidationError> {
        let storage = self.external_storage.borrow();
        for (published, start) in [(source, source_start), (destination, destination_start)] {
            let record = Self::checked_external_record(
                &storage,
                published,
                ExternalStorageKind::BoxedArray,
            )?;
            let end = start.checked_add(count).ok_or(
                ExternalStorageValidationError::IndexOutOfBounds {
                    index: start,
                    len: record.logical_len,
                },
            )?;
            if end > record.logical_len {
                return Err(ExternalStorageValidationError::IndexOutOfBounds {
                    index: end.saturating_sub(1),
                    len: record.logical_len,
                });
            }
        }
        let mut values = Vec::new();
        values
            .try_reserve_exact(count)
            .map_err(|_| ExternalStorageValidationError::BookkeepingAllocation)?;
        // Both complete ranges belong to authenticated active allocations. The
        // owned snapshot handles overlapping ranges without aliased Rust slices.
        let first = unsafe { source.add(8).cast::<*mut u8>().add(source_start) };
        for index in 0..count {
            values.push(unsafe { first.add(index).read() });
        }
        drop(storage);
        self.store_external_elements(destination, destination_start, &values)
    }

    /// The one-element prepared store shares the checked range owner.
    pub(crate) fn store_external_element(
        &self,
        published: *mut u8,
        index: usize,
        value: *mut u8,
    ) -> Result<(), ExternalStorageValidationError> {
        self.store_external_elements(published, index, &[value])
    }

    /// Store a fully checked byte range through the ledger owner. Bytes have
    /// no managed edges, but writes still advance the revision used to guard
    /// staged external sweep plans.
    pub(crate) fn store_external_bytes(
        &self,
        published: *mut u8,
        byte_offset: usize,
        bytes: &[u8],
    ) -> Result<(), ExternalStorageValidationError> {
        let storage = self.external_storage.borrow();
        let record =
            Self::checked_external_record(&storage, published, ExternalStorageKind::Bytes)?;
        let end = byte_offset.checked_add(bytes.len()).ok_or(
            ExternalStorageValidationError::IndexOutOfBounds {
                index: byte_offset,
                len: record.logical_len,
            },
        )?;
        if end > record.logical_len {
            return Err(ExternalStorageValidationError::IndexOutOfBounds {
                index: end.saturating_sub(1),
                len: record.logical_len,
            });
        }
        if !bytes.is_empty() {
            // The active record and checked complete span prove this copy stays
            // within its ledger-owned allocation. No safepoint intervenes.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bytes.as_ptr(),
                    published.add(8).add(byte_offset),
                    bytes.len(),
                );
            }
            self.external_changed();
        }
        Ok(())
    }

    /// Read a fully checked byte range through the ledger owner, giving reads
    /// the same owner-ledger authentication `store_external_bytes` gives
    /// writes rather than trusting a payload pointer obtained earlier.
    pub(crate) fn read_external_payload_offset(
        &self,
        published: *mut u8,
        byte_offset: usize,
        count: usize,
    ) -> Result<Vec<u8>, ExternalStorageValidationError> {
        let storage = self.external_storage.borrow();
        let data = Self::checked_external_byte_range(&storage, published, byte_offset, count)?;
        let mut copied = Vec::new();
        copied
            .try_reserve_exact(count)
            .map_err(|_| ExternalStorageValidationError::BookkeepingAllocation)?;
        if count != 0 {
            // The ledger borrow and checked complete span keep the allocation
            // active and in range for the entire owned snapshot.
            unsafe {
                std::ptr::copy_nonoverlapping(data, copied.as_mut_ptr(), count);
                copied.set_len(count);
            }
        }
        Ok(copied)
    }

    /// Copy a checked external byte span while sampling cancellation between
    /// bounded chunks. No heap action occurs while the ledger borrow is live.
    pub(crate) fn read_external_payload_offset_polling(
        &self,
        published: *mut u8,
        byte_offset: usize,
        count: usize,
    ) -> Result<Vec<u8>, RuntimeError> {
        let storage = self.external_storage.borrow();
        let data = Self::checked_external_byte_range(&storage, published, byte_offset, count)
            .map_err(|_| RuntimeError::BadPointer)?;
        let mut copied: Vec<u8> = Vec::new();
        copied
            .try_reserve_exact(count)
            .map_err(|_| RuntimeError::HeapOverflow)?;
        for offset in (0..count).step_by(4096) {
            if self.poll_prepared(crate::prepared_control::PreparedSafepoint::Backedge)
                != crate::prepared_control::CallStatus::Success
            {
                return Err(RuntimeError::Cancelled);
            }
            let chunk = (count - offset).min(4096);
            unsafe {
                std::ptr::copy_nonoverlapping(
                    data.add(offset),
                    copied.as_mut_ptr().add(offset),
                    chunk,
                );
                copied.set_len(offset + chunk);
            }
        }
        Ok(copied)
    }

    /// Authenticate a complete byte range. The returned pointer is usable only
    /// while the supplied ledger borrow remains live, with no callback or GC.
    fn checked_external_byte_range(
        storage: &HashMap<*mut u8, ExternalStorage>,
        published: *mut u8,
        offset: usize,
        count: usize,
    ) -> Result<*mut u8, ExternalStorageValidationError> {
        let record = Self::checked_external_record(storage, published, ExternalStorageKind::Bytes)?;
        let end =
            offset
                .checked_add(count)
                .ok_or(ExternalStorageValidationError::IndexOutOfBounds {
                    index: offset,
                    len: record.logical_len,
                })?;
        if end > record.logical_len {
            return Err(ExternalStorageValidationError::IndexOutOfBounds {
                index: end.saturating_sub(1),
                len: record.logical_len,
            });
        }
        Ok(unsafe { published.add(8).add(offset) })
    }

    /// Resolve an Addr# span through the existing allocation ledger, not by
    /// trusting a non-null address. Prefixes, boxed payloads, revoked storage,
    /// and ranges crossing allocation boundaries are never byte capabilities.
    /// The result must remain under this ledger borrow until the operation ends;
    /// generated callers separately keep the managed wrapper live across GC.
    fn external_address_span(
        storage: &HashMap<*mut u8, ExternalStorage>,
        address: usize,
        count: usize,
    ) -> Result<(*mut u8, usize), ExternalStorageValidationError> {
        for (&published, record) in storage {
            if record.kind != ExternalStorageKind::Bytes {
                continue;
            }
            let Some(start) = (published as usize).checked_add(8) else {
                continue;
            };
            let Some(offset) = address.checked_sub(start) else {
                continue;
            };
            if offset <= record.logical_len {
                Self::checked_external_byte_range(storage, published, offset, count)?;
                return Ok((published, offset));
            }
        }
        Err(ExternalStorageValidationError::Untracked(address))
    }

    /// Resolve a signed offset from an already authenticated address without
    /// allowing the offset to switch authority to a different allocation.
    fn external_address_offset_span(
        storage: &HashMap<*mut u8, ExternalStorage>,
        address: usize,
        offset: i64,
        count: usize,
    ) -> Result<(*mut u8, usize), ExternalStorageValidationError> {
        let (published, _) = Self::external_address_span(storage, address, 0)?;
        let record = Self::checked_external_record(storage, published, ExternalStorageKind::Bytes)?;
        let offset = isize::try_from(offset).map_err(|_| {
            ExternalStorageValidationError::IndexOutOfBounds {
                index: if offset.is_negative() { 0 } else { usize::MAX },
                len: record.logical_len,
            }
        })?;
        let target = address.checked_add_signed(offset).ok_or(
            ExternalStorageValidationError::IndexOutOfBounds {
                index: if offset.is_negative() { 0 } else { usize::MAX },
                len: record.logical_len,
            },
        )?;
        let start = (published as usize).checked_add(8).ok_or(
            ExternalStorageValidationError::SpanOverflow {
                kind: ExternalStorageKind::Bytes,
                logical_len: record.logical_len,
            },
        )?;
        let target_offset =
            target
                .checked_sub(start)
                .ok_or(ExternalStorageValidationError::IndexOutOfBounds {
                    index: 0,
                    len: record.logical_len,
                })?;
        Self::checked_external_byte_range(storage, published, target_offset, count)?;
        Ok((published, target_offset))
    }

    /// Produce the scalar address of an active byte payload after authenticating
    /// its published wrapper identity. The ledger remains the sole authority for
    /// every later dereference of the address.
    pub(crate) fn external_byte_address(
        &self,
        published: *mut u8,
    ) -> Result<usize, ExternalStorageValidationError> {
        let storage = self.external_storage.borrow();
        let record =
            Self::checked_external_record(&storage, published, ExternalStorageKind::Bytes)?;
        (published as usize)
            .checked_add(8)
            .ok_or(ExternalStorageValidationError::SpanOverflow {
                kind: ExternalStorageKind::Bytes,
                logical_len: record.logical_len,
            })
    }

    /// Get-or-insert `value` into the one permanent machine-wide literal
    /// pool, returning its (possibly freshly minted) pinned storage. Called
    /// while planning a compile against this machine: content this pool
    /// already carries returns the SAME storage (and therefore the same
    /// address) every earlier compile already baked into generated code;
    /// genuinely new content is minted and interned here, permanently, for
    /// the rest of this machine's life -- including when the compile that
    /// asked for it never ends up installed (see `absorb_interned_bytes`'s
    /// doc for why that leak is acceptable).
    pub(crate) fn interned_bytes(&self) -> Arc<crate::prepared_program::static_bytes::PinnedBytes> {
        Arc::clone(&self.interned_bytes.borrow())
    }

    /// Fold `other`'s literals into the permanent pool: every entry `other`
    /// carries that this pool does not already have (by content) joins it,
    /// in place, forever. Called once a program installs, absorbing its own
    /// compiled-in literal view (whether reused or newly minted at compile
    /// time -- see `interned_bytes`) into the one authority every program's
    /// generated `Addr#` accesses resolve against.
    ///
    /// Content is never removed once absorbed, even by a program that never
    /// finished installing or later retired: an interned literal is static
    /// data, indistinguishable in cost from a compiled program's own code,
    /// and reclaiming it would need tracking exactly which programs still
    /// reference which addresses -- the same cost this machine already
    /// avoids for code (see `tidepool-codegen/CLAUDE.md`'s "JIT allocation").
    pub(crate) fn absorb_interned_bytes(
        &self,
        other: &Arc<crate::prepared_program::static_bytes::PinnedBytes>,
    ) {
        let mut pool = self.interned_bytes.borrow_mut();
        if Arc::ptr_eq(&pool, other) {
            return;
        }
        // `Arc::make_mut` clones only when something else still holds this
        // exact pool snapshot (an outstanding compile's `existing_bytes`, or
        // an earlier install that reused it verbatim because it added
        // nothing -- see `PinnedBytes::overlay`/`absorb`'s docs); the common
        // case, a compile that minted a few new literals, finds this cell
        // the sole owner and grows the pool's `base` in place instead of
        // cloning every literal ever interned in this session. `PinnedBytes`
        // clones cheaply either way (its own fields are an `Arc` and an
        // empty `local` layer -- see its overlay doc); the potentially large
        // clone this guards is the inner `Tables` `absorb` mutates.
        let absorption = Arc::make_mut(&mut pool).absorb(other);
        pool.report_residency(absorption);
    }

    /// Resolve a literal `Addr#` against the one permanent pool.
    pub(crate) fn resolve_literal_bytes<R>(
        &self,
        mut resolve: impl FnMut(&crate::prepared_program::static_bytes::PinnedBytes) -> Option<R>,
    ) -> Option<R> {
        resolve(&self.interned_bytes.borrow())
    }

    /// Find a terminal NUL inside an authenticated byte array's logical extent.
    pub(crate) fn external_c_string_len(
        &self,
        address: usize,
    ) -> Result<usize, ExternalStorageValidationError> {
        let storage = self.external_storage.borrow();
        let (published, offset) = Self::external_address_span(&storage, address, 0)?;
        let record =
            Self::checked_external_record(&storage, published, ExternalStorageKind::Bytes)?;
        let remaining = record.logical_len - offset;
        // Authentication and the ledger borrow retain the entire bounded span.
        let bytes = unsafe { std::slice::from_raw_parts(published.add(8).add(offset), remaining) };
        bytes.iter().position(|byte| *byte == 0).ok_or(
            ExternalStorageValidationError::IndexOutOfBounds {
                index: record.logical_len,
                len: record.logical_len,
            },
        )
    }

    /// Snapshot a complete ledger-authenticated Addr# span. The ledger borrow
    /// covers validation and the copy; no raw payload pointer escapes it.
    pub(crate) fn read_external_address(
        &self,
        address: usize,
        count: usize,
    ) -> Result<Vec<u8>, ExternalStorageValidationError> {
        self.read_external_address_offset(address, 0, count)
    }

    /// Snapshot a signed-offset span while retaining the base address's
    /// original ledger authority.
    pub(crate) fn read_external_address_offset(
        &self,
        address: usize,
        offset: i64,
        count: usize,
    ) -> Result<Vec<u8>, ExternalStorageValidationError> {
        let storage = self.external_storage.borrow();
        let (published, offset) =
            Self::external_address_offset_span(&storage, address, offset, count)?;
        let mut copied = Vec::new();
        copied
            .try_reserve_exact(count)
            .map_err(|_| ExternalStorageValidationError::BookkeepingAllocation)?;
        if count != 0 {
            // The ledger borrow and checked span keep the allocation active and
            // in range for the entire owned snapshot.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    published.add(8).add(offset),
                    copied.as_mut_ptr(),
                    count,
                );
                copied.set_len(count);
            }
        }
        Ok(copied)
    }

    /// Store a complete ledger-authenticated Addr# span. Bytes contain no
    /// managed edges, but every nonempty mutation advances the sweep revision.
    pub(crate) fn store_external_address(
        &self,
        address: usize,
        bytes: &[u8],
    ) -> Result<(), ExternalStorageValidationError> {
        self.store_external_address_offset(address, 0, bytes)
    }

    /// Store a signed-offset span while retaining the base address's original
    /// ledger authority.
    pub(crate) fn store_external_address_offset(
        &self,
        address: usize,
        offset: i64,
        bytes: &[u8],
    ) -> Result<(), ExternalStorageValidationError> {
        let storage = self.external_storage.borrow();
        let (published, offset) =
            Self::external_address_offset_span(&storage, address, offset, bytes.len())?;
        if !bytes.is_empty() {
            // The ledger borrow and checked span cover the complete write; no
            // callback or collection can split authentication from mutation.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bytes.as_ptr(),
                    published.add(8).add(offset),
                    bytes.len(),
                );
            }
            self.external_changed();
        }
        Ok(())
    }

    /// GHC copyByteArray# forbids source/destination aliases. Validate both
    /// complete ranges and that precondition before any write. This is a
    /// noncollecting ledger mutation; bytes require no managed-edge barrier.
    pub(crate) fn copy_external_byte_range(
        &self,
        source: *mut u8,
        source_offset: usize,
        destination: *mut u8,
        destination_offset: usize,
        count: usize,
    ) -> Result<(), ExternalStorageValidationError> {
        self.copy_external_byte_range_with(
            source,
            source_offset,
            destination,
            destination_offset,
            count,
            ByteCopyAliasing::Disjoint,
        )
    }

    /// GHC copyMutableByteArray# permits the source and destination to be the
    /// same array with overlapping ranges (memmove semantics). Both complete
    /// ranges are still validated before any write.
    pub(crate) fn copy_external_byte_range_overlapping(
        &self,
        source: *mut u8,
        source_offset: usize,
        destination: *mut u8,
        destination_offset: usize,
        count: usize,
    ) -> Result<(), ExternalStorageValidationError> {
        self.copy_external_byte_range_with(
            source,
            source_offset,
            destination,
            destination_offset,
            count,
            ByteCopyAliasing::Overlapping,
        )
    }

    /// GHC setByteArray#: fill a complete active byte span with one byte.
    /// The range is validated before any write; this is a noncollecting
    /// ledger mutation.
    pub(crate) fn fill_external_byte_range(
        &self,
        array: *mut u8,
        offset: usize,
        count: usize,
        value: u8,
    ) -> Result<(), ExternalStorageValidationError> {
        let storage = self.external_storage.borrow();
        let to = Self::checked_external_byte_range(&storage, array, offset, count)?;
        if count != 0 {
            // The complete span was authenticated above, including activity.
            unsafe { std::ptr::write_bytes(to, value, count) };
            self.external_changed();
        }
        Ok(())
    }

    fn copy_external_byte_range_with(
        &self,
        source: *mut u8,
        source_offset: usize,
        destination: *mut u8,
        destination_offset: usize,
        count: usize,
        aliasing: ByteCopyAliasing,
    ) -> Result<(), ExternalStorageValidationError> {
        let storage = self.external_storage.borrow();
        let from = Self::checked_external_byte_range(&storage, source, source_offset, count)?;
        let to =
            Self::checked_external_byte_range(&storage, destination, destination_offset, count)?;
        if source == destination && aliasing == ByteCopyAliasing::Disjoint {
            return Err(ExternalStorageValidationError::AliasedByteCopy);
        }
        if count != 0 {
            // Both complete spans were authenticated before copying, including
            // their activity. Distinct ledger allocations are disjoint; the
            // overlapping policy only ever aliases within one allocation, and
            // `copy` handles that ordering.
            unsafe { std::ptr::copy(from, to, count) };
            self.external_changed();
        }
        Ok(())
    }

    /// Compare two complete active byte spans as unsigned bytes. This is a
    /// noncollecting, read-only ledger operation; aliases are valid here.
    pub(crate) fn compare_external_byte_ranges(
        &self,
        left: *mut u8,
        left_offset: usize,
        right: *mut u8,
        right_offset: usize,
        count: usize,
    ) -> Result<i64, ExternalStorageValidationError> {
        let storage = self.external_storage.borrow();
        let left = Self::checked_external_byte_range(&storage, left, left_offset, count)?;
        let right = Self::checked_external_byte_range(&storage, right, right_offset, count)?;
        let left = unsafe { std::slice::from_raw_parts(left, count) };
        let right = unsafe { std::slice::from_raw_parts(right, count) };
        Ok(match left.cmp(right) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        })
    }

    /// Find the first needle byte in one complete active byte span. This is a
    /// noncollecting, read-only ledger operation; the returned index is
    /// relative to the span's start, or -1 when the needle is absent.
    pub(crate) fn find_external_byte(
        &self,
        published: *mut u8,
        offset: usize,
        count: usize,
        needle: u8,
    ) -> Result<i64, ExternalStorageValidationError> {
        let storage = self.external_storage.borrow();
        let span = Self::checked_external_byte_range(&storage, published, offset, count)?;
        // The ledger borrow and checked span authenticate every scanned byte;
        // nothing is read outside them and no payload borrow escapes.
        let span = unsafe { std::slice::from_raw_parts(span, count) };
        Ok(span
            .iter()
            .position(|byte| *byte == needle)
            .map_or(-1, |index| index as i64))
    }

    /// text's `_hs_text_measure_off` over an authenticated byte span: walk
    /// UTF-8 lead bytes until `count` characters are found. Returns the bytes
    /// consumed when `count` characters fit, otherwise the negated number of
    /// characters found. Every lead byte read lies inside the checked span; a
    /// sequence whose continuation bytes would pass the span end still counts
    /// as one character, exactly as text's C kernel does, without reading them.
    pub(crate) fn measure_external_utf8(
        &self,
        published: *mut u8,
        offset: usize,
        length: usize,
        count: usize,
    ) -> Result<i64, ExternalStorageValidationError> {
        let storage = self.external_storage.borrow();
        let span = Self::checked_external_byte_range(&storage, published, offset, length)?;
        // SAFETY: the ledger borrow and checked span authenticate every byte.
        let span = unsafe { std::slice::from_raw_parts(span, length) };
        let (mut position, mut found) = (0_usize, 0_usize);
        while found < count && position < span.len() {
            position += match span[position] {
                byte if byte < 0xC0 => 1,
                byte if byte < 0xE0 => 2,
                byte if byte < 0xF0 => 3,
                _ => 4,
            };
            found += 1;
        }
        Ok(if found >= count {
            i64::try_from(position).unwrap_or(i64::MAX)
        } else {
            -i64::try_from(found).unwrap_or(i64::MAX)
        })
    }

    /// Snapshot an active byte payload while its ledger owner is borrowed.
    /// This call is noncollecting and returns owned storage; no payload borrow
    /// survives into later observation or forcing steps.
    pub(crate) fn copy_external_bytes(
        &self,
        published: *mut u8,
    ) -> Result<Vec<u8>, ExternalStorageValidationError> {
        let storage = self.external_storage.borrow();
        let record =
            Self::checked_external_record(&storage, published, ExternalStorageKind::Bytes)?;
        let len = record.logical_len;
        let mut copied = Vec::new();
        copied
            .try_reserve_exact(len)
            .map_err(|_| ExternalStorageValidationError::BookkeepingAllocation)?;
        if len != 0 {
            // SAFETY: the checked active record authenticates the complete
            // byte span; capacity was reserved before initializing the copy.
            unsafe {
                std::ptr::copy_nonoverlapping(published.add(8), copied.as_mut_ptr(), len);
                copied.set_len(len);
            }
        }
        Ok(copied)
    }

    /// Checked compare-and-swap with the same owner barrier as ordinary writes.
    pub(crate) fn compare_exchange_external_element(
        &self,
        published: *mut u8,
        index: usize,
        expected: *mut u8,
        value: *mut u8,
    ) -> Result<*mut u8, ExternalStorageValidationError> {
        let storage = self.external_storage.borrow();
        let record =
            Self::checked_external_record(&storage, published, ExternalStorageKind::BoxedArray)?;
        if index >= record.logical_len {
            return Err(ExternalStorageValidationError::IndexOutOfBounds {
                index,
                len: record.logical_len,
            });
        }
        // SAFETY: owner validation and index check prove this slot.
        let slot = unsafe { published.add(8).cast::<*mut u8>().add(index) };
        let old = unsafe { slot.read() };
        if old == expected {
            if record.generation == ExternalGeneration::Retained {
                let mut remembered = self.remembered_slots.borrow_mut();
                remembered
                    .try_reserve(1)
                    .map_err(|_| ExternalStorageValidationError::BookkeepingAllocation)?;
                remembered.insert(slot);
            }
            // No callback or safepoint splits this operation.
            unsafe { slot.write(value) };
            self.external_changed();
        }
        Ok(old)
    }

    /// Validate and reserve all bookkeeping first, then mark selected payloads
    /// Retained and remember their boxed slots.
    /// Called after both promotion copies succeed, before execution resumes.
    /// Failure at that point is incomplete promotion, never reusable.
    pub(crate) fn retain_external_payloads(
        &self,
        selected: &[(usize, ExternalStorageKind)],
    ) -> Result<(), ExternalStorageValidationError> {
        let mut storage = self.external_storage.borrow_mut();
        let mut slots = Vec::new();
        for &(address, kind) in selected {
            let published = address as *mut u8;
            let record = Self::structural_external_record(&storage, published, kind)?;
            if kind == ExternalStorageKind::BoxedArray {
                slots
                    .try_reserve(record.logical_len)
                    .map_err(|_| ExternalStorageValidationError::BookkeepingAllocation)?;
                for index in 0..record.logical_len {
                    // SAFETY: record validation proved the whole boxed span.
                    slots.push(unsafe { published.add(8).cast::<*mut u8>().add(index) });
                }
            }
        }
        let mut remembered = self.remembered_slots.borrow_mut();
        remembered
            .try_reserve(slots.len())
            .map_err(|_| ExternalStorageValidationError::BookkeepingAllocation)?;
        for (&published, record) in storage.iter_mut() {
            if selected
                .iter()
                .any(|&(address, _)| address == published as usize)
            {
                record.generation = ExternalGeneration::Retained;
            }
        }
        // Same sink as `register_remembered_slot` (inlined to reuse the
        // reserved borrow), so the test kill switch covers this recorder too.
        if !crate::host_fns::remembered_set_disabled_for_test() {
            for slot in slots {
                remembered.insert(slot);
            }
        }
        if !selected.is_empty() {
            self.external_changed();
        }
        Ok(())
    }

    /// Stage only unmarked Young allocations after the entire minor operation
    /// succeeds, never between growth recopies.
    pub(crate) fn plan_external_minor_sweep(
        &self,
        marked: &HashSet<*mut u8>,
    ) -> Result<ExternalSweepPlan, ExternalStorageValidationError> {
        let storage = self.external_storage.borrow();
        for &published in marked {
            if !storage.contains_key(&published) {
                return Err(ExternalStorageValidationError::Untracked(
                    published as usize,
                ));
            }
        }
        for (&published, record) in storage.iter() {
            Self::validate_external_record(published, record)?;
        }
        let dead = Self::stage_external_dead(&storage, marked, true)?;
        Ok(ExternalSweepPlan {
            revision: self
                .external_revision
                .get()
                .ok_or(ExternalStorageValidationError::LedgerChanged)?,
            allocated_objects: self.external_allocated_objects.get(),
            live_objects: storage.len(),
            dead,
        })
    }

    /// Allocate and own a prepared external payload before publishing its
    /// address. Ledger admission and layout arithmetic are fallible; after
    /// admission, no callback or collection splits ownership from prefix
    /// initialization.
    pub(crate) fn allocate_external_storage(
        &self,
        kind: ExternalStorageKind,
        logical_len: usize,
    ) -> Result<*mut u8, ExternalStorageValidationError> {
        if kind == ExternalStorageKind::Bytes {
            return self.allocate_external_bytes(logical_len, 8);
        }
        let size = logical_len
            .checked_mul(std::mem::size_of::<*mut u8>())
            .and_then(|bytes| 8usize.checked_add(bytes))
            .ok_or(ExternalStorageValidationError::SpanOverflow { kind, logical_len })?;
        let layout = Layout::from_size_align(size, 8)
            .map_err(|_| ExternalStorageValidationError::SpanOverflow { kind, logical_len })?;
        self.allocate_external_with_layout(kind, logical_len, layout, 0)
    }

    pub(crate) fn allocate_external_bytes(
        &self,
        logical_len: usize,
        alignment: usize,
    ) -> Result<*mut u8, ExternalStorageValidationError> {
        let (layout, published_offset) = aligned_byte_layout(logical_len, alignment)?;
        self.allocate_external_with_layout(
            ExternalStorageKind::Bytes,
            logical_len,
            layout,
            published_offset,
        )
    }

    fn allocate_external_with_layout(
        &self,
        kind: ExternalStorageKind,
        logical_len: usize,
        layout: Layout,
        published_offset: usize,
    ) -> Result<*mut u8, ExternalStorageValidationError> {
        self.external_storage
            .borrow_mut()
            .try_reserve(1)
            .map_err(|_| ExternalStorageValidationError::BookkeepingAllocation)?;
        // SAFETY: layout is nonempty and valid; the ledger owns the returned
        // allocation before any pointer is returned to a caller.
        let base = unsafe { std::alloc::alloc_zeroed(layout) };
        if base.is_null() {
            return Err(ExternalStorageValidationError::BookkeepingAllocation);
        }
        let published = unsafe { base.add(published_offset) };
        self.register_external_storage(
            published,
            base,
            layout,
            published_offset,
            kind,
            logical_len,
        );
        // SAFETY: both representations reserve their length prefix; byte
        // arrays additionally reserve the capacity prefix immediately before
        // the published handle. Alignment padding may precede that prefix.
        unsafe {
            if kind == ExternalStorageKind::Bytes {
                published
                    .sub(8)
                    .cast::<u64>()
                    .write((layout.size() - (published_offset - 8)) as u64);
            }
            published.cast::<u64>().write(logical_len as u64);
        }
        Ok(published)
    }

    /// Take ownership of a fresh allocation before its pointer is initialized
    /// or published to JIT code.
    pub(crate) fn register_external_storage(
        &self,
        published: *mut u8,
        base: *mut u8,
        layout: Layout,
        published_offset: usize,
        kind: ExternalStorageKind,
        logical_len: usize,
    ) {
        let old = self.external_storage.borrow_mut().insert(
            published,
            ExternalStorage {
                base,
                layout,
                published_offset,
                kind,
                logical_len,
                generation: ExternalGeneration::Young,
                activity: ExternalActivity::Active,
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
        self.external_changed();
    }

    #[cfg(test)]
    pub(crate) fn set_external_logical_len(&self, ptr: *mut u8, logical_len: usize) {
        if let Some(record) = self.external_storage.borrow_mut().get_mut(&ptr) {
            if record.logical_len != logical_len {
                let old_len = record.logical_len;
                let kind = record.kind;
                record.logical_len = logical_len;
                if kind == ExternalStorageKind::BoxedArray && logical_len < old_len {
                    let start = (ptr as usize) + 8 + logical_len * 8;
                    let end = (ptr as usize) + 8 + old_len * 8;
                    self.forget_remembered_range(start as *const u8, end as *const u8);
                }
                self.external_changed();
            }
        }
    }

    /// Replace an active byte payload with a fresh identity, preserving its
    /// prefix and zero-initializing growth. No collection or callback occurs.
    /// Allocation and validation precede revocation: failure leaves the old
    /// identity usable, while success leaves it structurally owned but revoked
    /// until a complete liveness trace permits reclamation.
    pub(crate) fn resize_external_bytes(
        &self,
        published: *mut u8,
        new_len: usize,
    ) -> Result<*mut u8, ExternalStorageValidationError> {
        let alignment = {
            let storage = self.external_storage.borrow();
            Self::checked_external_record(&storage, published, ExternalStorageKind::Bytes)?
                .layout
                .align()
        };
        let replacement = self.allocate_external_bytes(new_len, alignment)?;
        let mut storage = self.external_storage.borrow_mut();
        let old = storage
            .get_mut(&published)
            .ok_or(ExternalStorageValidationError::Untracked(
                published as usize,
            ))?;
        Self::validate_external_access(published, old, ExternalStorageKind::Bytes, true)?;
        let copy_len = old.logical_len.min(new_len);
        // Both allocations remain ledger-owned and disjoint. The replacement
        // was zero-initialized; only the common prefix requires a copy.
        if copy_len != 0 {
            unsafe {
                std::ptr::copy_nonoverlapping(published.add(8), replacement.add(8), copy_len);
            }
        }
        old.activity = ExternalActivity::Revoked;
        self.external_changed();
        Ok(replacement)
    }

    /// Logical shrink preserves the allocation and published identity, including
    /// its byte-array capacity prefix. All validation precedes mutation.
    pub(crate) fn shrink_external_payload(
        &self,
        published: *mut u8,
        kind: ExternalStorageKind,
        new_len: usize,
    ) -> Result<(), ExternalStorageValidationError> {
        let mut storage = self.external_storage.borrow_mut();
        let record =
            storage
                .get_mut(&published)
                .ok_or(ExternalStorageValidationError::Untracked(
                    published as usize,
                ))?;
        Self::validate_external_access(published, record, kind, true)?;
        let old_len = record.logical_len;
        if new_len > old_len {
            return Err(ExternalStorageValidationError::LengthIncrease {
                old: old_len,
                new: new_len,
            });
        }
        if new_len == old_len {
            return Ok(());
        }
        // SAFETY: validation proved the length prefix lies in the allocation.
        unsafe { published.cast::<u64>().write(new_len as u64) };
        record.logical_len = new_len;
        if kind == ExternalStorageKind::BoxedArray {
            let first = unsafe { published.add(8).cast::<*mut u8>().add(new_len) };
            let end = unsafe { published.add(8).cast::<*mut u8>().add(old_len) };
            self.forget_remembered_range(first.cast(), end.cast());
        }
        self.external_changed();
        Ok(())
    }

    /// Revoke mutator access while keeping the allocation in the ledger for
    /// structural validation and a later sweep.
    #[cfg(test)]
    pub(crate) fn revoke_external_payload(
        &self,
        published: *mut u8,
        kind: ExternalStorageKind,
    ) -> Result<(), ExternalStorageValidationError> {
        let mut storage = self.external_storage.borrow_mut();
        let record =
            storage
                .get_mut(&published)
                .ok_or(ExternalStorageValidationError::Untracked(
                    published as usize,
                ))?;
        Self::validate_external_access(published, record, kind, true)?;
        // Revocation does not retire GC edges: a live old wrapper may still
        // contain a nursery child that minor collection must evacuate.
        record.activity = ExternalActivity::Revoked;
        self.external_changed();
        Ok(())
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
        if !(record.base as usize).is_multiple_of(record.layout.align())
            || !(published as usize).is_multiple_of(prefix_alignment)
        {
            return Err(ExternalStorageValidationError::PointerAlignment { kind: record.kind });
        }
        let stored_len = match record.kind {
            ExternalStorageKind::Bytes => {
                if record.published_offset < 8 {
                    return Err(ExternalStorageValidationError::PublishedPointerMismatch {
                        kind: record.kind,
                    });
                }
                let expected_published = (record.base as usize)
                    .checked_add(record.published_offset)
                    .ok_or(ExternalStorageValidationError::SpanOverflow {
                        kind: record.kind,
                        logical_len: record.logical_len,
                    })?;
                if published as usize != expected_published {
                    return Err(ExternalStorageValidationError::PublishedPointerMismatch {
                        kind: record.kind,
                    });
                }
                let data_offset = record.published_offset.checked_add(8).ok_or(
                    ExternalStorageValidationError::SpanOverflow {
                        kind: record.kind,
                        logical_len: record.logical_len,
                    },
                )?;
                let required = data_offset.checked_add(record.logical_len).ok_or(
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
                if expected_published
                    .checked_add(8)
                    .is_none_or(|data| !data.is_multiple_of(record.layout.align()))
                {
                    return Err(ExternalStorageValidationError::PointerAlignment {
                        kind: record.kind,
                    });
                }
                // SAFETY: pointer relationship and required allocation span
                // were checked above before either prefix is read.
                let capacity_offset = record.published_offset - 8;
                let available = record.layout.size() - capacity_offset;
                let stored_capacity =
                    unsafe { *record.base.add(capacity_offset).cast::<u64>() } as usize;
                if stored_capacity != available {
                    return Err(ExternalStorageValidationError::CapacityPrefixMismatch {
                        recorded: available,
                        stored: stored_capacity,
                    });
                }
                // SAFETY: required >= 16, so the published length prefix is
                // within the registered allocation.
                (unsafe { *(published as *const u64) }) as usize
            }
            ExternalStorageKind::BoxedArray => {
                if record.published_offset != 0 || published != record.base {
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
            .ok_or(ExternalStorageValidationError::Untracked(
                published as usize,
            ))?;
        if record.kind != expected {
            return Err(ExternalStorageValidationError::KindMismatch {
                expected,
                actual: record.kind,
            });
        }
        Self::validate_external_record(published, record)?;
        let pointer_slots = if record.kind == ExternalStorageKind::BoxedArray {
            // SAFETY: validation proved the complete slot span is inside the
            // registered allocation. The span is a raw view, not a Rust
            // borrow; GC callers keep the ledger allocation owned until all
            // slot traversal and rewriting finishes, then commit any sweep.
            unsafe {
                tidepool_heap::external_storage::ExternalPointerSlots::from_validated(
                    published.add(8).cast(),
                    record.logical_len,
                )
            }
        } else {
            // SAFETY: an empty span never dereferences its base.  Keep the
            // representation explicit rather than manufacturing a Vec for
            // every byte payload view.
            unsafe {
                tidepool_heap::external_storage::ExternalPointerSlots::from_validated(
                    std::ptr::NonNull::<*mut u8>::dangling().as_ptr(),
                    0,
                )
            }
        };
        Ok(ExternalPayloadView {
            pointer_slots,
            logical_len: record.logical_len,
        })
    }

    /// Mutator and observation view. A revoked payload stays in the structural
    /// GC ledger, but cannot be exposed to ordinary operations.
    pub(crate) fn external_active_view(
        &self,
        published: *mut u8,
        expected: ExternalStorageKind,
    ) -> Result<ExternalPayloadView, ExternalStorageValidationError> {
        {
            let storage = self.external_storage.borrow();
            Self::checked_external_record(&storage, published, expected)?;
        }
        self.external_payload_view(published, expected)
    }

    fn stage_external_dead(
        storage: &HashMap<*mut u8, ExternalStorage>,
        marked: &HashSet<*mut u8>,
        young_only: bool,
    ) -> Result<Vec<*mut u8>, ExternalStorageValidationError> {
        let mut dead = Vec::new();
        dead.try_reserve(storage.len())
            .map_err(|_| ExternalStorageValidationError::BookkeepingAllocation)?;
        dead.extend(storage.iter().filter_map(|(&published, record)| {
            (!marked.contains(&published)
                && (!young_only || record.generation == ExternalGeneration::Young))
                .then_some(published)
        }));
        Ok(dead)
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
                return Err(ExternalStorageValidationError::Untracked(
                    published as usize,
                ));
            }
        }
        for (&published, record) in storage.iter() {
            Self::validate_external_record(published, record)?;
        }
        let dead = Self::stage_external_dead(&storage, marked, false)?;
        Ok(ExternalSweepPlan {
            revision: self
                .external_revision
                .get()
                .ok_or(ExternalStorageValidationError::LedgerChanged)?,
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
        if self.external_revision.get() != Some(plan.revision)
            || self.external_allocated_objects.get() != plan.allocated_objects
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
        self.external_changed();
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
/// that hold `vmctx` use this.
///
/// # Safety
/// `vmctx` must be non-null and `(*vmctx).machine_state` must have been
/// installed (by `PreparedMachine::install_registries`, or wired directly
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
/// `vmctx` is null or `(*vmctx).machine_state` is null, as in a hand-built
/// test context that does not install a machine.
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
/// `PreparedMachine::install_registries`; the returned previous value is
/// restored by `RegistryGuard::drop`. `pub` (not `pub(crate)`): bare-VMContext
/// test harnesses in `tests/` (separate crates, no `PreparedMachine`) call
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
/// it: the pointee is owned by a `PreparedMachine` whose run may end (and
/// clear `CURRENT_MACHINE`) at any safepoint.
pub(crate) unsafe fn current_machine<'a>() -> Option<&'a MachineState> {
    let p = CURRENT_MACHINE.with(|c| c.get());
    if p.is_null() {
        None
    } else {
        Some(&*p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nested failure bookkeeping must not panic when an earlier path still
    /// owns the first-cause cell.
    #[test]
    fn busy_runtime_error_cell_does_not_panic() {
        let ms = MachineState::new();
        let _guard = ms.runtime_error.borrow_mut();

        // set_first_cause: silently cannot write the cause, but does not panic.
        ms.set_first_cause(RuntimeError::Cancelled);
        // take_runtime_error: None, not a panic.
        assert_eq!(ms.take_runtime_error(), None);
    }

    #[test]
    fn first_cause_wins_while_integrity_disposition_is_monotonic() {
        let ms = MachineState::new();
        ms.set_first_cause(RuntimeError::Cancelled);
        // A reusable cause is this call's outcome only: nothing latches.
        assert_eq!(
            ms.current_failure(),
            Some(MachineFailure {
                cause: RuntimeError::Cancelled,
                disposition: MachineDisposition::Reusable,
            })
        );
        assert_eq!(ms.last_failure(), None);
        ms.set_first_cause(RuntimeError::BadPointer);

        // The call outcome keeps its first cause; the latch holds the first
        // INTEGRITY cause, which is what makes the machine unavailable.
        assert_eq!(ms.disposition(), MachineDisposition::Unavailable);
        assert_eq!(
            ms.last_failure(),
            Some(MachineFailure {
                cause: RuntimeError::BadPointer,
                disposition: MachineDisposition::Unavailable,
            })
        );
        assert_eq!(ms.current_failure(), ms.last_failure());
        assert_eq!(ms.take_runtime_error(), Some(RuntimeError::Cancelled));
        assert!(ms.begin_prepared_call().is_err(), "the latch never clears");
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
    fn prepared_raise_root_survives_observation_mark_cleanup() {
        use tidepool_heap::execution_descriptor::ObjectDescriptor;
        use tidepool_repr::execution_schema::{testing, StorageLayout};

        let machine = std::rc::Rc::new(MachineState::new());
        let descriptor = Arc::new(
            ObjectDescriptor::constructor(
                1,
                StorageLayout::for_reps(&testing::target(), &[]).unwrap(),
                None,
            )
            .unwrap(),
        );
        machine
            .install_prepared_buffer(
                vec![descriptor.initial_header_word() as u64, 0],
                vec![descriptor],
            )
            .unwrap();
        let reference = machine.gc_active_range().unwrap().0;
        let mark = machine.rust_roots_mark();
        let mut temporary = reference;
        machine.register_rust_root(&mut temporary);
        // The descriptor-backed object and stable Rc machine remain owned.
        unsafe { machine.record_prepared_raise(reference) };
        machine.truncate_rust_roots(mark);
        let mut roots = Vec::new();
        machine.extend_rust_roots(&mut roots);
        assert_eq!(roots, vec![machine.prepared_exception.as_ptr()]);
        assert_eq!(machine.prepared_exception.get(), reference);
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
        assert_eq!(
            machine.take_runtime_error(),
            Some(RuntimeError::RaisedException)
        );
        assert!(machine.prepared_exception.get().is_null());
    }

    #[test]
    fn removing_an_outer_temporary_root_preserves_nested_mark_cleanup() {
        let machine = MachineState::new();
        let mut outer = std::ptr::null_mut();
        let outer_registration = machine.register_rust_root(&mut outer);
        let nested_mark = machine.rust_roots_mark();
        let mut nested = std::ptr::null_mut();
        machine.register_rust_root(&mut nested);

        machine.deregister_rust_root(outer_registration, &mut outer);
        machine.truncate_rust_roots(nested_mark);

        let mut roots = Vec::new();
        machine.extend_rust_roots(&mut roots);
        assert!(roots.is_empty());
    }

    fn prepared_tag_fixture(
        family_tag: u32,
    ) -> (
        MachineState,
        Arc<tidepool_heap::execution_descriptor::ObjectDescriptor>,
        usize,
    ) {
        use tidepool_heap::execution_descriptor::ObjectDescriptor;
        use tidepool_repr::execution_schema::{testing, StorageLayout};

        let descriptor = Arc::new(
            ObjectDescriptor::constructor(
                family_tag,
                StorageLayout::for_reps(&testing::target(), &[]).unwrap(),
                None,
            )
            .unwrap(),
        );
        let extent = descriptor.allocation_extent() as usize;
        let machine = MachineState::new();
        machine
            .install_prepared_buffer(
                vec![0_u64; extent.div_ceil(8)],
                vec![Arc::clone(&descriptor)],
            )
            .unwrap();
        let address = machine.gc_active_range().unwrap().0;
        unsafe { descriptor.initialize_header(address) };
        (machine, descriptor, address as usize)
    }

    #[test]
    fn prepared_constructor_tag_uses_full_family_identity_with_raw_and_seven_evidence() {
        for family_tag in [1_u32, 2, 6, 7, 8, 42] {
            let (machine, descriptor, address) = prepared_tag_fixture(family_tag);
            let expected = i64::from(family_tag - 1);
            assert_eq!(
                unsafe { machine.prepared_constructor_tag(address) },
                Ok(expected)
            );
            assert_eq!(
                unsafe { machine.prepared_constructor_tag(address | 7) },
                Ok(expected)
            );
            assert_eq!(
                unsafe {
                    machine.prepared_constructor_tag(address | usize::from(descriptor.tag()))
                },
                Ok(expected)
            );
        }
    }

    #[test]
    fn prepared_constructor_tag_rejects_contradictory_low_bits_and_null() {
        let (machine, _, address) = prepared_tag_fixture(2);
        assert_eq!(
            unsafe { machine.prepared_constructor_tag(address | 1) },
            Err(RuntimeError::BadPointer)
        );
        let (machine, _, address) = prepared_tag_fixture(8);
        assert_eq!(
            unsafe { machine.prepared_constructor_tag(address | 1) },
            Err(RuntimeError::BadPointer)
        );
        for encoded in [0, 7] {
            assert_eq!(
                unsafe { machine.prepared_constructor_tag(encoded) },
                Err(RuntimeError::BadPointer)
            );
        }
    }

    #[test]
    fn prepared_constructor_tag_rejects_function_and_nonlive_headers() {
        use tidepool_heap::execution_descriptor::{DescriptorState, ObjectDescriptor, ObjectKind};
        use tidepool_repr::execution_schema::{testing, StorageLayout};

        let function = Arc::new(
            ObjectDescriptor::new(
                ObjectKind::Function,
                StorageLayout::for_reps(&testing::target(), &[]).unwrap(),
                None,
            )
            .unwrap(),
        );
        let machine = MachineState::new();
        machine
            .install_prepared_buffer(vec![0_u64; 2], vec![Arc::clone(&function)])
            .unwrap();
        let address = machine.gc_active_range().unwrap().0;
        unsafe { function.initialize_header(address) };
        let error = unsafe { machine.prepared_constructor_tag(address as usize | 7) }.unwrap_err();
        assert_eq!(error, RuntimeError::ExpectedConstructor);
        assert_eq!(error.machine_disposition(), MachineDisposition::Unavailable);

        let (machine, descriptor, address) = prepared_tag_fixture(1);
        for state in [
            DescriptorState::Forwarded,
            DescriptorState::Evaluating,
            DescriptorState::Updated,
        ] {
            unsafe {
                (address as *mut usize).write(descriptor.initial_header_word() | state as usize)
            };
            assert_eq!(
                unsafe { machine.prepared_constructor_tag(address | 1) },
                Err(RuntimeError::BadThunkState(state as u8))
            );
        }
        unsafe { (address as *mut usize).write(usize::MAX & !7) };
        assert_eq!(
            unsafe { machine.prepared_constructor_tag(address) },
            Err(RuntimeError::BadPointer)
        );
    }

    fn prepared_exception_fixture(
        object_count: usize,
    ) -> (
        std::rc::Rc<MachineState>,
        Arc<tidepool_heap::execution_descriptor::ObjectDescriptor>,
    ) {
        use tidepool_heap::execution_descriptor::ObjectDescriptor;
        use tidepool_repr::execution_schema::{testing, StorageLayout};

        let machine = std::rc::Rc::new(MachineState::new());
        let descriptor = Arc::new(
            ObjectDescriptor::constructor(
                1,
                StorageLayout::for_reps(&testing::target(), &[]).unwrap(),
                None,
            )
            .unwrap(),
        );
        let extent = descriptor.allocation_extent() as usize;
        let mut buffer = vec![0_u64; (extent * object_count).div_ceil(8)];
        for index in 0..object_count {
            // SAFETY: every offset names one complete allocation in `buffer`.
            unsafe {
                descriptor.initialize_header(buffer.as_mut_ptr().cast::<u8>().add(index * extent));
            }
        }
        machine
            .install_prepared_buffer(buffer, vec![Arc::clone(&descriptor)])
            .unwrap();
        machine
            .gc_state
            .borrow_mut()
            .as_mut()
            .unwrap()
            .prepared
            .as_mut()
            .unwrap()
            .used = extent * object_count;
        (machine, descriptor)
    }

    fn prepared_exception_reference(
        machine: &MachineState,
        descriptor: &tidepool_heap::execution_descriptor::ObjectDescriptor,
        index: usize,
    ) -> *mut u8 {
        let start = machine.gc_active_range().unwrap().0;
        // SAFETY: the fixture initialized an object at every descriptor-sized offset.
        unsafe { start.add(index * descriptor.allocation_extent() as usize) }
    }

    #[test]
    fn first_prepared_raise_wins_and_keeps_its_operand() {
        let (machine, descriptor) = prepared_exception_fixture(2);
        let first = prepared_exception_reference(&machine, &descriptor, 0);
        let second = prepared_exception_reference(&machine, &descriptor, 1);

        // SAFETY: both references are exact starts in the fixture's admitted nursery.
        unsafe { machine.record_prepared_raise(first) };
        // SAFETY: the second reference is also an admitted exact start.
        unsafe { machine.record_prepared_raise(second) };

        assert_eq!(machine.prepared_exception.get(), first);
        assert_eq!(
            machine.take_runtime_error(),
            Some(RuntimeError::RaisedException)
        );
        assert!(machine.prepared_exception.get().is_null());
    }

    #[test]
    fn earlier_cancellation_prevents_prepared_exception_capture() {
        let (machine, descriptor) = prepared_exception_fixture(1);
        let reference = prepared_exception_reference(&machine, &descriptor, 0);
        machine.set_first_cause(RuntimeError::Cancelled);

        // SAFETY: `reference` is an admitted exact start in the fixture nursery.
        unsafe { machine.record_prepared_raise(reference) };

        assert!(machine.prepared_exception.get().is_null());
        assert_eq!(machine.take_runtime_error(), Some(RuntimeError::Cancelled));
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
    }

    #[test]
    fn later_bad_pointer_preserves_exception_cause_and_upgrades_disposition() {
        let (machine, descriptor) = prepared_exception_fixture(1);
        let reference = prepared_exception_reference(&machine, &descriptor, 0);

        // SAFETY: `reference` is an admitted exact start in the fixture nursery.
        unsafe { machine.record_prepared_raise(reference) };
        machine.set_first_cause(RuntimeError::BadPointer);

        assert_eq!(machine.prepared_exception.get(), reference);
        assert_eq!(machine.disposition(), MachineDisposition::Unavailable);
        assert_eq!(
            machine.take_runtime_error(),
            Some(RuntimeError::RaisedException)
        );
        assert!(machine.prepared_exception.get().is_null());
    }

    #[test]
    fn collector_rewrites_independent_prepared_exception_root() {
        let (machine, descriptor) = prepared_exception_fixture(1);
        let before = prepared_exception_reference(&machine, &descriptor, 0);
        // SAFETY: `before` is an admitted exact start in the fixture nursery.
        unsafe { machine.record_prepared_raise(before) };

        let mut state = machine.take_gc_state().unwrap();
        let old_buffer = state.active_buffer.take().unwrap();
        let from_start = state.active_start;
        let from_used = state.prepared.as_ref().unwrap().used;
        let mut to_buffer = vec![0_u64; old_buffer.len()];
        let to_len = std::mem::size_of_val(to_buffer.as_slice());
        let roots = vec![machine.prepared_exception.as_ptr()];
        let copied = unsafe {
            let to_bytes =
                std::slice::from_raw_parts_mut(to_buffer.as_mut_ptr().cast::<u8>(), to_len);
            tidepool_heap::gc::raw::cheney_copy_descriptors(
                &roots,
                from_start,
                from_used,
                to_bytes,
                &mut state.prepared.as_mut().unwrap().space,
            )
        }
        .unwrap();
        assert_eq!(copied.bytes_copied, descriptor.allocation_extent() as usize);
        let after = machine.prepared_exception.get();
        assert_ne!(after, before);
        assert!(after as usize >= to_buffer.as_ptr() as usize);
        assert!((after as usize) < to_buffer.as_ptr() as usize + to_len);
        state.active_start = to_buffer.as_mut_ptr().cast();
        state.active_size = to_len;
        state.active_buffer = Some(to_buffer);
        machine.put_gc_state(state);

        assert_eq!(
            machine.prepared_call_status(),
            crate::prepared_control::CallStatus::LanguageFailure
        );
        assert_eq!(
            machine.take_runtime_error(),
            Some(RuntimeError::RaisedException)
        );
    }

    #[test]
    fn heap_teardown_clears_exception_slot_without_consuming_cause() {
        let (machine, descriptor) = prepared_exception_fixture(1);
        let reference = prepared_exception_reference(&machine, &descriptor, 0);
        // SAFETY: `reference` is an admitted exact start in the fixture nursery.
        unsafe { machine.record_prepared_raise(reference) };

        machine.clear_run_scratch();

        assert!(machine.prepared_exception.get().is_null());
        assert_eq!(
            machine.take_runtime_error(),
            Some(RuntimeError::RaisedException)
        );
    }

    #[test]
    fn complete_root_snapshot_joins_every_registry() {
        let ms = MachineState::new();
        let mut stack_value: *mut u8 = std::ptr::without_provenance_mut(1);
        let mut rust_value = 2usize as *mut u8;
        let mut persistent_value = 3usize as *mut u8;
        let mut stowed_value = 4usize as *mut u8;
        let mut remembered_value = 5usize as *mut u8;
        let mut code_value = 6usize as *mut u8;

        ms.register_rust_root(&mut rust_value);
        ms.register_persistent_root(&mut persistent_value);
        ms.register_stowed_root(&mut stowed_value);
        ms.register_code_roots([&mut code_value as *mut *mut u8]);
        ms.register_remembered_slot(&mut remembered_value);

        // SAFETY: every argument is the stable address of a live local pointer
        // slot for the duration of this assertion.
        let slots = unsafe { ms.complete_root_snapshot(&[&mut stack_value]) }.into_slots();

        assert_eq!(slots.len(), 6);
        for expected in [
            &mut stack_value as *mut *mut u8,
            &mut rust_value,
            &mut persistent_value,
            &mut stowed_value,
            &mut code_value,
            &mut remembered_value,
        ] {
            assert!(slots.contains(&expected));
        }
    }

    #[test]
    fn major_snapshot_keeps_remembered_edges_distinct_from_strong_roots() {
        let ms = MachineState::new();
        let mut persistent: *mut u8 = std::ptr::without_provenance_mut(1);
        let mut remembered = 2usize as *mut u8;
        ms.register_persistent_root(&mut persistent);
        ms.register_remembered_slot(&mut remembered);

        let slots = unsafe { ms.complete_root_snapshot(&[]) }.into_major_slots();
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
        ms.register_external_storage(published, base, layout, published_offset, kind, logical_len);
        published
    }

    #[test]
    fn prepared_external_allocation_owns_zero_length_payloads_and_rejects_overflow() {
        let ms = MachineState::new();
        let bytes = ms
            .allocate_external_storage(ExternalStorageKind::Bytes, 0)
            .unwrap();
        let boxed = ms
            .allocate_external_storage(ExternalStorageKind::BoxedArray, 0)
            .unwrap();
        assert_eq!(unsafe { bytes.sub(8).cast::<u64>().read() }, 16);
        assert_eq!(unsafe { bytes.cast::<u64>().read() }, 0);
        assert_eq!(unsafe { boxed.cast::<u64>().read() }, 0);
        assert!(ms
            .external_payload_view(bytes, ExternalStorageKind::Bytes)
            .unwrap()
            .pointer_slots
            .is_empty());
        assert!(ms
            .external_payload_view(boxed, ExternalStorageKind::BoxedArray)
            .unwrap()
            .pointer_slots
            .is_empty());
        let before = ms.external_storage_stats();
        assert!(matches!(
            ms.allocate_external_storage(ExternalStorageKind::Bytes, usize::MAX),
            Err(ExternalStorageValidationError::SpanOverflow { .. })
        ));
        assert!(matches!(
            ms.allocate_external_storage(ExternalStorageKind::BoxedArray, usize::MAX),
            Err(ExternalStorageValidationError::SpanOverflow { .. })
        ));
        assert_eq!(ms.external_storage_stats(), before);
    }

    #[test]
    fn aligned_byte_allocations_authenticate_prefix_geometry_and_requested_power() {
        let ms = MachineState::new();
        for alignment in [1, 8, 16, 32, 4096] {
            let published = ms.allocate_external_bytes(5, alignment).unwrap();
            let address = ms.external_byte_address(published).unwrap();
            assert_eq!(address % alignment, 0);
            let storage = ms.external_storage.borrow();
            let record = &storage[&published];
            assert_eq!(
                published as usize,
                record.base as usize + record.published_offset
            );
            assert_eq!(record.layout.align(), alignment.max(8));
            assert_eq!(unsafe { published.sub(8).cast::<u64>().read() }, 21);
            assert_eq!(unsafe { published.cast::<u64>().read() }, 5);
        }
        for alignment in [0, 3, 6] {
            assert!(matches!(
                ms.allocate_external_bytes(1, alignment),
                Err(ExternalStorageValidationError::LayoutAlignment { actual })
                    if actual == alignment
            ));
        }
    }

    #[test]
    fn aligned_byte_validation_rejects_corrupt_prefix_and_recorded_offset() {
        let ms = MachineState::new();
        let published = ms.allocate_external_bytes(4, 64).unwrap();
        let capacity = unsafe { published.sub(8).cast::<u64>().read() };
        unsafe { published.sub(8).cast::<u64>().write(capacity + 1) };
        assert!(matches!(
            ms.external_active_view(published, ExternalStorageKind::Bytes),
            Err(ExternalStorageValidationError::CapacityPrefixMismatch { .. })
        ));
        unsafe { published.sub(8).cast::<u64>().write(capacity) };

        let offset = ms.external_storage.borrow()[&published].published_offset;
        ms.external_storage
            .borrow_mut()
            .get_mut(&published)
            .unwrap()
            .published_offset = offset + 8;
        assert!(matches!(
            ms.external_active_view(published, ExternalStorageKind::Bytes),
            Err(ExternalStorageValidationError::PublishedPointerMismatch { .. })
        ));
        ms.external_storage
            .borrow_mut()
            .get_mut(&published)
            .unwrap()
            .published_offset = offset;
    }

    #[test]
    fn aligned_byte_resize_preserves_alignment_and_shrink_preserves_capacity() {
        let ms = MachineState::new();
        let old = ms.allocate_external_bytes(4, 256).unwrap();
        ms.store_external_bytes(old, 0, b"abcd").unwrap();
        let replacement = ms.resize_external_bytes(old, 7).unwrap();
        assert_eq!(ms.external_byte_address(replacement).unwrap() % 256, 0);
        assert_eq!(ms.copy_external_bytes(replacement).unwrap(), b"abcd\0\0\0");
        assert!(matches!(
            ms.external_active_view(old, ExternalStorageKind::Bytes),
            Err(ExternalStorageValidationError::Revoked(_))
        ));
        let capacity = unsafe { replacement.sub(8).cast::<u64>().read() };
        ms.shrink_external_payload(replacement, ExternalStorageKind::Bytes, 2)
            .unwrap();
        assert_eq!(unsafe { replacement.sub(8).cast::<u64>().read() }, capacity);
        assert_eq!(unsafe { replacement.cast::<u64>().read() }, 2);
        assert_eq!(ms.external_byte_address(replacement).unwrap() % 256, 0);
    }

    #[test]
    fn w5_external_lifetime_young_writes_are_not_roots_and_retention_remembers_slots() {
        let ms = MachineState::new();
        let payload = unsafe { register_test_external(&ms, ExternalStorageKind::BoxedArray, 2) };
        ms.store_external_element(payload, 0, std::ptr::null_mut())
            .unwrap();
        assert_eq!(ms.remembered_slots_count(), 0);
        ms.retain_external_payloads(&[(payload as usize, ExternalStorageKind::BoxedArray)])
            .unwrap();
        assert_eq!(ms.remembered_slots_count(), 2);
        let dead_young = unsafe { register_test_external(&ms, ExternalStorageKind::Bytes, 3) };
        ms.commit_external_sweep(ms.plan_external_minor_sweep(&HashSet::new()).unwrap())
            .unwrap();
        assert!(ms.external_storage.borrow().contains_key(&payload));
        assert!(!ms.external_storage.borrow().contains_key(&dead_young));
    }

    #[test]
    fn w5_bulk_boxed_copy_supports_overlapping_ranges_in_both_directions() {
        let ms = MachineState::new();
        let payload = ms
            .allocate_external_storage(ExternalStorageKind::BoxedArray, 5)
            .unwrap();
        let values = (1..=5)
            .map(|value| value as usize as *mut u8)
            .collect::<Vec<_>>();
        ms.store_external_elements(payload, 0, &values).unwrap();

        // Source starts before destination: snapshotting is required to avoid
        // reading values already overwritten by the forward overlap.
        ms.copy_external_elements(payload, 0, payload, 1, 4)
            .unwrap();
        let slots = unsafe { std::slice::from_raw_parts(payload.add(8).cast::<*mut u8>(), 5) };
        assert_eq!(slots, [1usize, 1, 2, 3, 4].map(|value| value as *mut u8));

        // Source starts after destination: the same owner must preserve the
        // reverse overlap rather than behaving like a forward-only copy.
        ms.copy_external_elements(payload, 1, payload, 0, 4)
            .unwrap();
        let slots = unsafe { std::slice::from_raw_parts(payload.add(8).cast::<*mut u8>(), 5) };
        assert_eq!(slots, [1usize, 2, 3, 4, 4].map(|value| value as *mut u8));
    }

    #[test]
    fn w5_bulk_boxed_copy_rejects_incomplete_spans_without_revision_or_write() {
        let ms = MachineState::new();
        let source = ms
            .allocate_external_storage(ExternalStorageKind::BoxedArray, 3)
            .unwrap();
        let destination = ms
            .allocate_external_storage(ExternalStorageKind::BoxedArray, 3)
            .unwrap();
        let source_values = [1usize, 2, 3].map(|value| value as *mut u8);
        let destination_values = [9usize, 9, 9].map(|value| value as *mut u8);
        ms.store_external_elements(source, 0, &source_values)
            .unwrap();
        ms.store_external_elements(destination, 0, &destination_values)
            .unwrap();

        for (source_start, destination_start) in [(2, 0), (0, 2)] {
            let before = ms.external_revision.get();
            assert!(matches!(
                ms.copy_external_elements(source, source_start, destination, destination_start, 2),
                Err(ExternalStorageValidationError::IndexOutOfBounds { .. })
            ));
            assert_eq!(ms.external_revision.get(), before);
            let slots =
                unsafe { std::slice::from_raw_parts(destination.add(8).cast::<*mut u8>(), 3) };
            assert_eq!(slots, [9usize, 9, 9].map(|value| value as *mut u8));
        }
    }

    #[test]
    fn w5_bulk_boxed_copy_accepts_zero_length_endpoint_without_revision() {
        let ms = MachineState::new();
        let payload = ms
            .allocate_external_storage(ExternalStorageKind::BoxedArray, 3)
            .unwrap();
        let values = [1usize, 2, 3].map(|value| value as *mut u8);
        ms.store_external_elements(payload, 0, &values).unwrap();
        let before = ms.external_revision.get();

        ms.copy_external_elements(payload, 3, payload, 3, 0)
            .unwrap();

        assert_eq!(ms.external_revision.get(), before);
        let slots = unsafe { std::slice::from_raw_parts(payload.add(8).cast::<*mut u8>(), 3) };
        assert_eq!(slots, values);
    }

    #[test]
    fn w5_bulk_boxed_copy_remembers_retained_destination_slots() {
        let ms = MachineState::new();
        let source = ms
            .allocate_external_storage(ExternalStorageKind::BoxedArray, 3)
            .unwrap();
        let destination = ms
            .allocate_external_storage(ExternalStorageKind::BoxedArray, 3)
            .unwrap();
        let source_values = [1usize, 2, 3].map(|value| value as *mut u8);
        ms.store_external_elements(source, 0, &source_values)
            .unwrap();
        ms.retain_external_payloads(&[(destination as usize, ExternalStorageKind::BoxedArray)])
            .unwrap();
        // Isolate the copy's barrier admission from the retention transition.
        ms.clear_remembered_slots();
        let before = ms.external_revision.get();

        ms.copy_external_elements(source, 0, destination, 1, 2)
            .unwrap();

        assert_eq!(ms.remembered_slots_count(), 2);
        assert_ne!(ms.external_revision.get(), before);
        let slots = unsafe { std::slice::from_raw_parts(destination.add(8).cast::<*mut u8>(), 3) };
        assert_eq!(slots, [0usize, 1, 2].map(|value| value as *mut u8));
    }

    #[test]
    fn external_retention_validates_every_payload_before_generation_or_roots_change() {
        let ms = MachineState::new();
        let boxed = unsafe { register_test_external(&ms, ExternalStorageKind::BoxedArray, 2) };
        let byte = unsafe { register_test_external(&ms, ExternalStorageKind::Bytes, 1) };
        let before = ms.external_revision.get();
        assert!(matches!(
            ms.retain_external_payloads(&[
                (boxed as usize, ExternalStorageKind::BoxedArray),
                (byte as usize, ExternalStorageKind::BoxedArray),
            ]),
            Err(ExternalStorageValidationError::KindMismatch { .. })
        ));
        assert_eq!(ms.remembered_slots_count(), 0);
        assert_eq!(ms.external_revision.get(), before);
        assert!(ms
            .plan_external_minor_sweep(&HashSet::new())
            .unwrap()
            .dead
            .contains(&boxed));
    }

    #[test]
    fn external_mutations_reject_bad_identity_kind_length_and_bounds_without_changes() {
        let ms = MachineState::new();
        let boxed = unsafe { register_test_external(&ms, ExternalStorageKind::BoxedArray, 2) };
        let bytes = unsafe { register_test_external(&ms, ExternalStorageKind::Bytes, 3) };
        let value = 7usize as *mut u8;
        let before = ms.external_revision.get();
        assert!(matches!(
            ms.store_external_element(boxed.wrapping_add(1), 0, value),
            Err(ExternalStorageValidationError::Untracked(_))
        ));
        assert!(matches!(
            ms.store_external_element(bytes, 0, value),
            Err(ExternalStorageValidationError::KindMismatch { .. })
        ));
        assert!(matches!(
            ms.store_external_elements(boxed, 1, &[value, value]),
            Err(ExternalStorageValidationError::IndexOutOfBounds { .. })
        ));
        assert!(matches!(
            ms.compare_exchange_external_element(boxed, 2, std::ptr::null_mut(), value),
            Err(ExternalStorageValidationError::IndexOutOfBounds { .. })
        ));
        assert!(matches!(
            ms.shrink_external_payload(boxed, ExternalStorageKind::BoxedArray, 3),
            Err(ExternalStorageValidationError::LengthIncrease { .. })
        ));
        assert_eq!(ms.external_revision.get(), before);
        assert_eq!(ms.remembered_slots_count(), 0);
        assert_eq!(
            unsafe { boxed.add(8).cast::<*mut u8>().read() },
            std::ptr::null_mut()
        );
        assert_eq!(unsafe { boxed.cast::<u64>().read() }, 2);
    }

    #[test]
    fn malformed_external_prefix_blocks_lifetime_changes_without_partial_mutation() {
        let ms = MachineState::new();
        let boxed = unsafe { register_test_external(&ms, ExternalStorageKind::BoxedArray, 2) };
        let before = ms.external_revision.get();
        unsafe { boxed.cast::<u64>().write(3) };
        assert!(matches!(
            ms.retain_external_payloads(&[(boxed as usize, ExternalStorageKind::BoxedArray)]),
            Err(ExternalStorageValidationError::LogicalLengthMismatch { .. })
        ));
        assert!(matches!(
            ms.shrink_external_payload(boxed, ExternalStorageKind::BoxedArray, 1),
            Err(ExternalStorageValidationError::LogicalLengthMismatch { .. })
        ));
        assert!(matches!(
            ms.revoke_external_payload(boxed, ExternalStorageKind::BoxedArray),
            Err(ExternalStorageValidationError::LogicalLengthMismatch { .. })
        ));
        assert_eq!(ms.external_revision.get(), before);
        assert_eq!(ms.remembered_slots_count(), 0);
        let record = ms.external_storage.borrow();
        assert_eq!(record[&boxed].generation, ExternalGeneration::Young);
        assert_eq!(record[&boxed].activity, ExternalActivity::Active);
        assert_eq!(record[&boxed].logical_len, 2);
    }

    #[test]
    fn checked_range_and_cas_remember_only_retained_slots() {
        let ms = MachineState::new();
        let boxed = unsafe { register_test_external(&ms, ExternalStorageKind::BoxedArray, 2) };
        let first = 7usize as *mut u8;
        let second = 9usize as *mut u8;
        ms.store_external_elements(boxed, 0, &[first, second])
            .unwrap();
        assert_eq!(ms.remembered_slots_count(), 0);
        assert_eq!(
            ms.compare_exchange_external_element(boxed, 0, second, second)
                .unwrap(),
            first
        );
        ms.retain_external_payloads(&[(boxed as usize, ExternalStorageKind::BoxedArray)])
            .unwrap();
        ms.clear_remembered_slots();
        assert_eq!(
            ms.compare_exchange_external_element(boxed, 0, first, second)
                .unwrap(),
            first
        );
        assert_eq!(ms.remembered_slots_count(), 1);
        ms.store_external_elements(boxed, 0, &[first, first])
            .unwrap();
        assert_eq!(ms.remembered_slots_count(), 2);
    }

    #[test]
    fn external_shrink_preserves_identity_and_capacity_and_forgets_tail() {
        let ms = MachineState::new();
        let boxed = unsafe { register_test_external(&ms, ExternalStorageKind::BoxedArray, 3) };
        let bytes = unsafe { register_test_external(&ms, ExternalStorageKind::Bytes, 4) };
        ms.retain_external_payloads(&[(boxed as usize, ExternalStorageKind::BoxedArray)])
            .unwrap();
        assert_eq!(ms.remembered_slots_count(), 3);
        ms.shrink_external_payload(boxed, ExternalStorageKind::BoxedArray, 1)
            .unwrap();
        assert_eq!(ms.remembered_slots_count(), 1);
        assert_eq!(
            ms.external_payload_view(boxed, ExternalStorageKind::BoxedArray)
                .unwrap()
                .pointer_slots
                .len(),
            1
        );
        ms.shrink_external_payload(bytes, ExternalStorageKind::Bytes, 2)
            .unwrap();
        assert_eq!(unsafe { bytes.cast::<u64>().read() }, 2);
        assert_eq!(unsafe { bytes.sub(8).cast::<u64>().read() }, 20);
        assert!(ms.external_storage.borrow().contains_key(&boxed));
        assert!(ms.external_storage.borrow().contains_key(&bytes));
    }

    #[test]
    fn resize_growth_preserves_prefix_and_revokes_old() {
        let ms = MachineState::new();
        let old = ms
            .allocate_external_storage(ExternalStorageKind::Bytes, 3)
            .unwrap();
        ms.store_external_bytes(old, 0, b"abc").unwrap();
        let replacement = ms.resize_external_bytes(old, 6).unwrap();

        assert_ne!(replacement, old);
        assert_eq!(unsafe { replacement.sub(8).cast::<u64>().read() }, 22);
        assert_eq!(unsafe { replacement.cast::<u64>().read() }, 6);
        assert_eq!(ms.copy_external_bytes(replacement).unwrap(), b"abc\0\0\0");
        assert!(matches!(
            ms.external_active_view(old, ExternalStorageKind::Bytes),
            Err(ExternalStorageValidationError::Revoked(_))
        ));
        assert!(matches!(
            ms.store_external_bytes(old, 0, b"x"),
            Err(ExternalStorageValidationError::Revoked(_))
        ));
        assert_eq!(
            ms.external_payload_view(old, ExternalStorageKind::Bytes)
                .unwrap()
                .logical_len,
            3
        );
        assert_eq!(ms.external_storage.borrow().len(), 2);
    }

    #[test]
    fn byte_copy_rejects_aliases_without_mutation() {
        let ms = MachineState::new();
        let bytes = ms
            .allocate_external_storage(ExternalStorageKind::Bytes, 4)
            .unwrap();
        ms.store_external_bytes(bytes, 0, b"abcd").unwrap();
        assert_eq!(
            ms.copy_external_byte_range(bytes, 0, bytes, 1, 3),
            Err(ExternalStorageValidationError::AliasedByteCopy)
        );
        assert_eq!(ms.copy_external_bytes(bytes).unwrap(), b"abcd");
    }

    #[test]
    fn byte_fill_writes_only_the_validated_span() {
        let ms = MachineState::new();
        let bytes = ms
            .allocate_external_storage(ExternalStorageKind::Bytes, 4)
            .unwrap();
        ms.store_external_bytes(bytes, 0, b"abcd").unwrap();
        ms.fill_external_byte_range(bytes, 1, 2, b'z').unwrap();
        assert_eq!(ms.copy_external_bytes(bytes).unwrap(), b"azzd");
        ms.fill_external_byte_range(bytes, 4, 0, b'q').unwrap();
        assert_eq!(ms.copy_external_bytes(bytes).unwrap(), b"azzd");
        assert!(matches!(
            ms.fill_external_byte_range(bytes, 3, 2, b'q'),
            Err(ExternalStorageValidationError::IndexOutOfBounds { .. })
        ));
        assert_eq!(ms.copy_external_bytes(bytes).unwrap(), b"azzd");
    }

    #[test]
    fn overlapping_byte_copy_moves_within_one_array_in_both_directions() {
        let ms = MachineState::new();
        let bytes = ms
            .allocate_external_storage(ExternalStorageKind::Bytes, 4)
            .unwrap();
        ms.store_external_bytes(bytes, 0, b"abcd").unwrap();
        ms.copy_external_byte_range_overlapping(bytes, 0, bytes, 1, 3)
            .unwrap();
        assert_eq!(ms.copy_external_bytes(bytes).unwrap(), b"aabc");
        ms.copy_external_byte_range_overlapping(bytes, 1, bytes, 0, 3)
            .unwrap();
        assert_eq!(ms.copy_external_bytes(bytes).unwrap(), b"abcc");
        assert!(matches!(
            ms.copy_external_byte_range_overlapping(bytes, 2, bytes, 0, 3),
            Err(ExternalStorageValidationError::IndexOutOfBounds { .. })
        ));
        assert_eq!(ms.copy_external_bytes(bytes).unwrap(), b"abcc");
    }

    #[test]
    fn byte_copy_checks_both_complete_ranges_before_writing() {
        let ms = MachineState::new();
        let source = ms
            .allocate_external_storage(ExternalStorageKind::Bytes, 4)
            .unwrap();
        let destination = ms
            .allocate_external_storage(ExternalStorageKind::Bytes, 4)
            .unwrap();
        ms.store_external_bytes(source, 0, b"a\xffcd").unwrap();
        ms.store_external_bytes(destination, 0, b"zzzz").unwrap();

        for (source_offset, destination_offset, count) in [(3, 0, 2), (0, 3, 2)] {
            let before = ms.external_revision.get();
            assert!(matches!(
                ms.copy_external_byte_range(
                    source,
                    source_offset,
                    destination,
                    destination_offset,
                    count,
                ),
                Err(ExternalStorageValidationError::IndexOutOfBounds { .. })
            ));
            assert_eq!(ms.external_revision.get(), before);
            assert_eq!(ms.copy_external_bytes(destination).unwrap(), b"zzzz");
        }

        let before = ms.external_revision.get();
        ms.copy_external_byte_range(source, 4, destination, 4, 0)
            .unwrap();
        assert_eq!(ms.external_revision.get(), before);
        ms.copy_external_byte_range(source, 1, destination, 1, 2)
            .unwrap();
        assert_eq!(ms.copy_external_bytes(destination).unwrap(), b"z\xffcz");
        assert_ne!(ms.external_revision.get(), before);
    }

    #[test]
    fn byte_compare_is_unsigned_alias_safe_and_read_only() {
        let ms = MachineState::new();
        let left = ms
            .allocate_external_storage(ExternalStorageKind::Bytes, 3)
            .unwrap();
        let right = ms
            .allocate_external_storage(ExternalStorageKind::Bytes, 3)
            .unwrap();
        ms.store_external_bytes(left, 0, &[0x80, 0xff, 1]).unwrap();
        ms.store_external_bytes(right, 0, &[0x7f, 0xff, 2]).unwrap();
        let before = ms.external_revision.get();

        assert_eq!(ms.compare_external_byte_ranges(left, 0, right, 0, 3), Ok(1));
        assert_eq!(
            ms.compare_external_byte_ranges(right, 0, left, 0, 3),
            Ok(-1)
        );
        assert_eq!(ms.compare_external_byte_ranges(left, 1, right, 1, 1), Ok(0));
        assert_eq!(ms.compare_external_byte_ranges(left, 0, left, 0, 3), Ok(0));
        assert_eq!(ms.compare_external_byte_ranges(left, 3, left, 3, 0), Ok(0));
        assert!(matches!(
            ms.compare_external_byte_ranges(left, 2, right, 0, 2),
            Err(ExternalStorageValidationError::IndexOutOfBounds { .. })
        ));
        assert_eq!(ms.external_revision.get(), before);
    }

    #[test]
    fn external_strings_respect_logical_extents_and_utf8_continuations() {
        let machine = MachineState::new();
        let payload = machine.allocate_external_bytes(4, 8).unwrap();
        machine
            .store_external_bytes(payload, 0, &[0x80, b'a', 0, b'z'])
            .unwrap();
        let address = machine.external_byte_address(payload).unwrap();
        assert_eq!(machine.external_c_string_len(address).unwrap(), 2);
        assert_eq!(machine.external_c_string_len(address + 2).unwrap(), 0);
        assert!(machine.external_c_string_len(address + 3).is_err());
        assert!(machine.external_c_string_len(0).is_err());
        assert_eq!(machine.measure_external_utf8(payload, 0, 4, 1).unwrap(), 1);
        machine
            .shrink_external_payload(payload, ExternalStorageKind::Bytes, 2)
            .unwrap();
        assert!(machine.external_c_string_len(address).is_err());
    }

    #[test]
    fn external_address_contents_roundtrip_through_authenticated_byte_span() {
        let ms = MachineState::new();
        let published = ms
            .allocate_external_storage(ExternalStorageKind::Bytes, 6)
            .unwrap();
        let address = ms.external_byte_address(published).unwrap();
        let before = ms.external_revision.get();

        assert_eq!(address, published as usize + 8);
        ms.store_external_address(address + 1, b"tide").unwrap();

        assert_ne!(ms.external_revision.get(), before);
        assert_eq!(ms.read_external_address(address, 6).unwrap(), b"\0tide\0");
        assert_eq!(ms.read_external_address(address + 2, 2).unwrap(), b"id");
    }

    #[test]
    fn external_address_offsets_allow_negative_interior_ranges() {
        let ms = MachineState::new();
        let published = ms
            .allocate_external_storage(ExternalStorageKind::Bytes, 6)
            .unwrap();
        ms.store_external_bytes(published, 0, b"abcdef").unwrap();
        let address = ms.external_byte_address(published).unwrap();

        assert_eq!(
            ms.read_external_address_offset(address + 4, -3, 3).unwrap(),
            b"bcd"
        );
        ms.store_external_address_offset(address + 5, -2, b"XY")
            .unwrap();
        assert_eq!(ms.read_external_address(address, 6).unwrap(), b"abcXYf");
        assert!(matches!(
            ms.read_external_address_offset(address + 1, -2, 0),
            Err(ExternalStorageValidationError::IndexOutOfBounds { .. })
        ));
        assert!(matches!(
            ms.read_external_address_offset(address + 5, 2, 0),
            Err(ExternalStorageValidationError::IndexOutOfBounds { .. })
        ));
    }

    #[test]
    fn external_address_offsets_cannot_cross_into_another_owner_or_overflow() {
        let ms = MachineState::new();
        let first = ms
            .allocate_external_storage(ExternalStorageKind::Bytes, 4)
            .unwrap();
        let second = ms
            .allocate_external_storage(ExternalStorageKind::Bytes, 4)
            .unwrap();
        ms.store_external_bytes(first, 0, b"aaaa").unwrap();
        ms.store_external_bytes(second, 0, b"bbbb").unwrap();
        let first_address = ms.external_byte_address(first).unwrap();
        let second_address = ms.external_byte_address(second).unwrap();
        let (base, target) = if first_address < second_address {
            (first_address, second_address)
        } else {
            (second_address, first_address)
        };
        let cross_owner = i64::try_from(target - base).unwrap();
        let before = ms.external_revision.get();

        assert!(matches!(
            ms.read_external_address_offset(base, cross_owner, 1),
            Err(ExternalStorageValidationError::IndexOutOfBounds { .. })
        ));
        assert!(matches!(
            ms.store_external_address_offset(base, cross_owner, b"x"),
            Err(ExternalStorageValidationError::IndexOutOfBounds { .. })
        ));
        assert!(matches!(
            ms.read_external_address_offset(base, i64::MAX, 1),
            Err(ExternalStorageValidationError::IndexOutOfBounds { .. })
        ));
        assert!(matches!(
            ms.read_external_address_offset(base, i64::MIN, 1),
            Err(ExternalStorageValidationError::IndexOutOfBounds { .. })
        ));
        assert_eq!(ms.external_revision.get(), before);
        assert_eq!(ms.copy_external_bytes(first).unwrap(), b"aaaa");
        assert_eq!(ms.copy_external_bytes(second).unwrap(), b"bbbb");
    }

    #[test]
    fn external_addresses_reject_prefixes_boxed_untracked_and_revoked_storage() {
        let ms = MachineState::new();
        let published = ms
            .allocate_external_storage(ExternalStorageKind::Bytes, 2)
            .unwrap();
        let address = ms.external_byte_address(published).unwrap();
        let boxed = ms
            .allocate_external_storage(ExternalStorageKind::BoxedArray, 1)
            .unwrap();

        for prefix in [published as usize - 8, published as usize] {
            assert!(matches!(
                ms.read_external_address(prefix, 0),
                Err(ExternalStorageValidationError::Untracked(value)) if value == prefix
            ));
        }
        assert!(matches!(
            ms.external_byte_address(boxed),
            Err(ExternalStorageValidationError::KindMismatch { .. })
        ));
        assert!(matches!(
            ms.read_external_address(boxed as usize + 8, 0),
            Err(ExternalStorageValidationError::Untracked(_))
        ));
        assert!(matches!(
            ms.read_external_address(1, 0),
            Err(ExternalStorageValidationError::Untracked(1))
        ));

        ms.revoke_external_payload(published, ExternalStorageKind::Bytes)
            .unwrap();
        let before = ms.external_revision.get();
        assert!(matches!(
            ms.external_byte_address(published),
            Err(ExternalStorageValidationError::Revoked(_))
        ));
        assert!(matches!(
            ms.read_external_address(address, 1),
            Err(ExternalStorageValidationError::Revoked(_))
        ));
        assert!(matches!(
            ms.store_external_address(address, b"x"),
            Err(ExternalStorageValidationError::Revoked(_))
        ));
        assert_eq!(ms.external_revision.get(), before);
    }

    #[test]
    fn external_address_bounds_and_overflow_fail_before_mutation_but_empty_end_is_valid() {
        let ms = MachineState::new();
        let published = ms
            .allocate_external_storage(ExternalStorageKind::Bytes, 5)
            .unwrap();
        ms.store_external_bytes(published, 0, b"abcde").unwrap();
        let address = ms.external_byte_address(published).unwrap();
        let before = ms.external_revision.get();

        assert!(matches!(
            ms.read_external_address(address + 4, 2),
            Err(ExternalStorageValidationError::IndexOutOfBounds { .. })
        ));
        assert!(matches!(
            ms.store_external_address(address + 4, b"xy"),
            Err(ExternalStorageValidationError::IndexOutOfBounds { .. })
        ));
        assert!(matches!(
            ms.read_external_address(address + 1, usize::MAX),
            Err(ExternalStorageValidationError::IndexOutOfBounds { .. })
        ));
        assert_eq!(ms.read_external_address(address + 5, 0).unwrap(), b"");
        ms.store_external_address(address + 5, b"").unwrap();
        assert_eq!(ms.external_revision.get(), before);
        assert_eq!(ms.copy_external_bytes(published).unwrap(), b"abcde");
    }

    #[test]
    fn external_address_capability_tracks_logical_shrink_and_rejects_resized_old_identity() {
        let ms = MachineState::new();
        let published = ms
            .allocate_external_storage(ExternalStorageKind::Bytes, 4)
            .unwrap();
        ms.store_external_bytes(published, 0, b"abcd").unwrap();
        let address = ms.external_byte_address(published).unwrap();

        ms.shrink_external_payload(published, ExternalStorageKind::Bytes, 2)
            .unwrap();
        assert_eq!(ms.read_external_address(address + 2, 0).unwrap(), b"");
        assert!(matches!(
            ms.read_external_address(address + 2, 1),
            Err(ExternalStorageValidationError::IndexOutOfBounds { .. })
        ));
        assert!(matches!(
            ms.read_external_address(address + 3, 0),
            Err(ExternalStorageValidationError::Untracked(_))
        ));

        let replacement = ms.resize_external_bytes(published, 1).unwrap();
        assert!(matches!(
            ms.read_external_address(address, 1),
            Err(ExternalStorageValidationError::Revoked(_))
        ));
        let replacement_address = ms.external_byte_address(replacement).unwrap();
        assert_eq!(
            ms.read_external_address(replacement_address, 1).unwrap(),
            b"a"
        );
    }

    #[test]
    fn byte_copy_and_compare_reject_revoked_ranges_without_mutation() {
        let ms = MachineState::new();
        let source = ms
            .allocate_external_storage(ExternalStorageKind::Bytes, 2)
            .unwrap();
        let destination = ms
            .allocate_external_storage(ExternalStorageKind::Bytes, 2)
            .unwrap();
        ms.store_external_bytes(source, 0, b"ab").unwrap();
        ms.store_external_bytes(destination, 0, b"zz").unwrap();
        ms.revoke_external_payload(source, ExternalStorageKind::Bytes)
            .unwrap();
        let before = ms.external_revision.get();

        assert!(matches!(
            ms.copy_external_byte_range(source, 0, destination, 0, 2),
            Err(ExternalStorageValidationError::Revoked(_))
        ));
        assert!(matches!(
            ms.compare_external_byte_ranges(source, 0, destination, 0, 2),
            Err(ExternalStorageValidationError::Revoked(_))
        ));
        assert_eq!(ms.copy_external_bytes(destination).unwrap(), b"zz");
        assert_eq!(ms.external_revision.get(), before);
    }

    #[test]
    fn resize_shrink_and_equal_return_fresh_identity() {
        for new_len in [2, 4] {
            let ms = MachineState::new();
            let old = ms
                .allocate_external_storage(ExternalStorageKind::Bytes, 4)
                .unwrap();
            ms.store_external_bytes(old, 0, b"abcd").unwrap();
            let replacement = ms.resize_external_bytes(old, new_len).unwrap();

            assert_ne!(replacement, old);
            assert_eq!(
                ms.copy_external_bytes(replacement).unwrap(),
                &b"abcd"[..new_len]
            );
            assert_eq!(
                ms.external_active_view(replacement, ExternalStorageKind::Bytes)
                    .unwrap()
                    .logical_len,
                new_len
            );
            assert!(matches!(
                ms.external_active_view(old, ExternalStorageKind::Bytes),
                Err(ExternalStorageValidationError::Revoked(_))
            ));
        }
    }

    #[test]
    fn resize_failure_preserves_active_old() {
        let ms = MachineState::new();
        let old = ms
            .allocate_external_storage(ExternalStorageKind::Bytes, 4)
            .unwrap();
        let boxed = ms
            .allocate_external_storage(ExternalStorageKind::BoxedArray, 1)
            .unwrap();
        ms.store_external_bytes(old, 0, b"abcd").unwrap();
        let before_stats = ms.external_storage_stats();
        let before_revision = ms.external_revision.get();
        for (pointer, new_len) in [(old.wrapping_add(1), 2), (boxed, 2), (old, usize::MAX)] {
            assert!(ms.resize_external_bytes(pointer, new_len).is_err());
            assert_eq!(ms.external_storage_stats(), before_stats);
            assert_eq!(ms.external_revision.get(), before_revision);
            assert_eq!(ms.copy_external_bytes(old).unwrap(), b"abcd");
            assert_eq!(
                ms.external_active_view(old, ExternalStorageKind::Bytes)
                    .unwrap()
                    .logical_len,
                4
            );
        }
        unsafe { old.cast::<u64>().write(5) };
        assert!(matches!(
            ms.resize_external_bytes(old, 2),
            Err(ExternalStorageValidationError::LogicalLengthMismatch { .. })
        ));
        unsafe { old.cast::<u64>().write(4) };
        assert_eq!(ms.external_storage_stats(), before_stats);
        assert_eq!(ms.external_revision.get(), before_revision);
        assert_eq!(ms.copy_external_bytes(old).unwrap(), b"abcd");
    }

    #[test]
    fn resized_old_is_structurally_traceable_until_full_sweep_reclaims_it() {
        let ms = MachineState::new();
        let old = ms
            .allocate_external_storage(ExternalStorageKind::Bytes, 2)
            .unwrap();
        ms.store_external_bytes(old, 0, b"ab").unwrap();
        let replacement = ms.resize_external_bytes(old, 3).unwrap();

        ms.retain_external_payloads(&[(old as usize, ExternalStorageKind::Bytes)])
            .unwrap();
        assert_eq!(
            ms.external_payload_view(old, ExternalStorageKind::Bytes)
                .unwrap()
                .logical_len,
            2
        );
        ms.commit_external_sweep(
            ms.plan_external_minor_sweep(&HashSet::from([replacement]))
                .unwrap(),
        )
        .unwrap();
        assert!(ms.external_storage.borrow().contains_key(&old));
        assert!(ms.external_storage.borrow().contains_key(&replacement));
        ms.commit_external_sweep(
            ms.plan_external_sweep(&HashSet::from([replacement]))
                .unwrap(),
        )
        .unwrap();
        assert!(!ms.external_storage.borrow().contains_key(&old));
        assert_eq!(ms.copy_external_bytes(replacement).unwrap(), b"ab\0");
    }

    #[test]
    fn revoked_external_payload_remains_sweepable_but_rejects_views_and_writes() {
        let ms = MachineState::new();
        let boxed = unsafe { register_test_external(&ms, ExternalStorageKind::BoxedArray, 1) };
        ms.retain_external_payloads(&[(boxed as usize, ExternalStorageKind::BoxedArray)])
            .unwrap();
        ms.revoke_external_payload(boxed, ExternalStorageKind::BoxedArray)
            .unwrap();
        assert_eq!(ms.remembered_slots_count(), 1);
        assert_eq!(
            ms.external_payload_view(boxed, ExternalStorageKind::BoxedArray)
                .unwrap()
                .pointer_slots
                .len(),
            1
        );
        assert!(matches!(
            ms.external_active_view(boxed, ExternalStorageKind::BoxedArray),
            Err(ExternalStorageValidationError::Revoked(_))
        ));
        assert!(matches!(
            ms.store_external_element(boxed, 0, std::ptr::null_mut()),
            Err(ExternalStorageValidationError::Revoked(_))
        ));
        assert!(matches!(
            ms.shrink_external_payload(boxed, ExternalStorageKind::BoxedArray, 0),
            Err(ExternalStorageValidationError::Revoked(_))
        ));
        assert!(ms
            .plan_external_sweep(&HashSet::new())
            .unwrap()
            .dead
            .contains(&boxed));
    }

    #[test]
    fn revoked_young_payload_can_be_retained_for_structural_gc_reachability() {
        let ms = MachineState::new();
        let boxed = unsafe { register_test_external(&ms, ExternalStorageKind::BoxedArray, 2) };
        ms.revoke_external_payload(boxed, ExternalStorageKind::BoxedArray)
            .unwrap();
        ms.retain_external_payloads(&[(boxed as usize, ExternalStorageKind::BoxedArray)])
            .unwrap();
        assert_eq!(ms.remembered_slots_count(), 2);
        assert_eq!(
            ms.external_payload_view(boxed, ExternalStorageKind::BoxedArray)
                .unwrap()
                .pointer_slots
                .len(),
            2
        );
        assert!(matches!(
            ms.external_active_view(boxed, ExternalStorageKind::BoxedArray),
            Err(ExternalStorageValidationError::Revoked(_))
        ));
        assert!(matches!(
            ms.store_external_element(boxed, 0, std::ptr::null_mut()),
            Err(ExternalStorageValidationError::Revoked(_))
        ));
        ms.commit_external_sweep(ms.plan_external_minor_sweep(&HashSet::new()).unwrap())
            .unwrap();
        assert!(ms.external_storage.borrow().contains_key(&boxed));
    }

    #[test]
    fn every_external_ledger_mutation_invalidates_a_sweep_plan() {
        for mutation in 0..8 {
            let ms = MachineState::new();
            let boxed = unsafe { register_test_external(&ms, ExternalStorageKind::BoxedArray, 2) };
            let bytes = (mutation == 7)
                .then(|| unsafe { register_test_external(&ms, ExternalStorageKind::Bytes, 8) });
            let mut marked = HashSet::from([boxed]);
            if let Some(bytes) = bytes {
                marked.insert(bytes);
            }
            let plan = ms.plan_external_sweep(&marked).unwrap();
            match mutation {
                0 => {
                    unsafe { register_test_external(&ms, ExternalStorageKind::Bytes, 1) };
                }
                1 => {
                    ms.store_external_element(boxed, 0, std::ptr::null_mut())
                        .unwrap();
                }
                2 => {
                    ms.retain_external_payloads(&[(
                        boxed as usize,
                        ExternalStorageKind::BoxedArray,
                    )])
                    .unwrap();
                }
                3 => {
                    ms.shrink_external_payload(boxed, ExternalStorageKind::BoxedArray, 1)
                        .unwrap();
                }
                4 => {
                    ms.revoke_external_payload(boxed, ExternalStorageKind::BoxedArray)
                        .unwrap();
                }
                5 => {
                    assert!(ms.release_external_storage(boxed));
                }
                6 => {
                    ms.set_external_logical_len(boxed, 1);
                }
                7 => {
                    let bytes = bytes.unwrap();
                    ms.store_external_bytes(bytes, 0, &7_i64.to_ne_bytes())
                        .unwrap();
                    assert_eq!(unsafe { bytes.add(8).cast::<i64>().read_unaligned() }, 7);
                }
                _ => unreachable!(),
            }
            assert!(
                matches!(
                    ms.commit_external_sweep(plan),
                    Err(ExternalStorageValidationError::LedgerChanged)
                ),
                "mutation {mutation}"
            );
        }
    }

    #[test]
    fn exhausted_external_revision_permanently_forbids_sweep_plans() {
        let ms = MachineState::new();
        let boxed = unsafe { register_test_external(&ms, ExternalStorageKind::BoxedArray, 1) };
        ms.external_revision.set(Some(u64::MAX));
        ms.store_external_element(boxed, 0, std::ptr::null_mut())
            .unwrap();
        assert_eq!(ms.external_revision.get(), None);
        assert!(matches!(
            ms.plan_external_minor_sweep(&HashSet::new()),
            Err(ExternalStorageValidationError::LedgerChanged)
        ));
        assert!(matches!(
            ms.plan_external_sweep(&HashSet::new()),
            Err(ExternalStorageValidationError::LedgerChanged)
        ));
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
        let slots: Vec<_> = view.pointer_slots.into_iter().collect();
        assert_eq!(slots.len(), 2);
        assert_eq!(slots[0], unsafe { boxed.add(8) as *mut *mut u8 });
        assert_eq!(slots[1], unsafe { boxed.add(16) as *mut *mut u8 });
        // SAFETY: the helper registers the exact byte-array layout.
        let bytes = unsafe { register_test_external(&ms, ExternalStorageKind::Bytes, 3) };
        let bytes_view = ms
            .external_payload_view(bytes, ExternalStorageKind::Bytes)
            .unwrap();
        assert!(bytes_view.pointer_slots.is_empty());
        assert_eq!(bytes_view.pointer_slots.into_iter().next(), None);
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
        ms.install_prepared_buffer(vec![0_u64; 16], Vec::new())
            .unwrap();

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

    /// Teardown remains safe while `GcState` is temporarily taken out of its
    /// cell, as it is throughout `perform_gc`.
    #[test]
    fn gc_state_taken_out_leaves_cell_empty_and_teardown_is_safe() {
        let ms = MachineState::new();
        ms.install_prepared_buffer(vec![0_u64; 16], Vec::new())
            .unwrap();

        let _abandoned = ms.take_gc_state(); // never put back

        assert_eq!(ms.reclaim_session_heap(std::ptr::null_mut()), (None, 0));
        ms.clear_run_scratch();
        ms.free_session_heap();
    }

    /// `deregister_persistent_roots` must remove exactly the registrations
    /// that calling `deregister_persistent_root` once per listed address
    /// would remove — including when an address was registered more than
    /// once (each registration is a separate list slot; "remove-by-position,
    /// first match" per call). These addresses are never dereferenced by
    /// either function, only compared and stored, so dangling/fake values are
    /// fine here.
    #[test]
    fn deregister_persistent_roots_matches_single_root_semantics_with_duplicates() {
        let a = 0x1000_usize as *mut *mut u8;
        let b = 0x2000_usize as *mut *mut u8;
        let c = 0x3000_usize as *mut *mut u8;
        let d = 0x4000_usize as *mut *mut u8;

        // `a` is registered twice (distinct slots that happen to share an
        // address), `b` once, `c` once, `d` twice.
        let ms = MachineState::new();
        ms.register_persistent_root(a);
        ms.register_persistent_root(b);
        ms.register_persistent_root(a);
        ms.register_persistent_root(c);
        ms.register_persistent_root(d);
        ms.register_persistent_root(d);
        assert_eq!(ms.persistent_roots_count(), 6);

        // List `a` twice (removes both registrations), `b` once (removes its
        // one registration), and `d` once (removes only ONE of its two
        // registrations, matching what one `deregister_persistent_root(d)`
        // call would do) — `c` is left untouched.
        ms.deregister_persistent_roots(&[a, b, a, d]);

        let mut remaining = Vec::new();
        ms.extend_persistent_roots(&mut remaining);
        assert_eq!(remaining, vec![c, d], "one `d` registration must survive");

        // Cross-check against the single-root function on a twin machine
        // built the same way, driven one call per listed address in the same
        // order.
        let twin = MachineState::new();
        twin.register_persistent_root(a);
        twin.register_persistent_root(b);
        twin.register_persistent_root(a);
        twin.register_persistent_root(c);
        twin.register_persistent_root(d);
        twin.register_persistent_root(d);
        for slot in [a, b, a, d] {
            twin.deregister_persistent_root(slot);
        }
        let mut twin_remaining = Vec::new();
        twin.extend_persistent_roots(&mut twin_remaining);
        assert_eq!(remaining, twin_remaining);

        // An address listed but never registered, or listed more times than
        // it was registered, is a no-op for the surplus (idempotent, like
        // the single-root version).
        let unregistered = 0x5000_usize as *mut *mut u8;
        ms.deregister_persistent_roots(&[unregistered, c, c]);
        let mut after = Vec::new();
        ms.extend_persistent_roots(&mut after);
        assert_eq!(after, vec![d]);

        // Empty input is a no-op.
        ms.deregister_persistent_roots(&[]);
        let mut after_empty = Vec::new();
        ms.extend_persistent_roots(&mut after_empty);
        assert_eq!(after_empty, vec![d]);
    }
}
