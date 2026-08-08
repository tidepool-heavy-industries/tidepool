//! The high-level JIT effect machine ([`JitEffectMachine`]) and the effect-drive
//! loop at the JIT↔Rust boundary.
//!
//! # Nested child runs on a suspended machine (segment 40)
//!
//! A parent turn suspended at a typed yield (`runLLMTurn`/`Ask`) can host an
//! arbitrary number of SEQUENTIAL child fragment runs — including ones that
//! force GC and heap doubling — and resume correctly afterward. Two invariants
//! make this memory-safe:
//!
//! 1. **Registered-root replaces the temporal argument.** The stowed continuation
//!    used to be safe because "no GC runs on a suspended machine" (enforced by
//!    the L7 `suspended_continuation.is_none()` asserts on every run entry).
//!    While a child runs, that is no longer true — the child allocates and
//!    collects. [`JitEffectMachine::enter_nested_child`] MOVES the continuation
//!    pointer out of `suspended_continuation` into a heap-stable `Box` cell and
//!    registers that cell's address in the machine's `stowed_roots` set, which
//!    `perform_gc` folds into its root assembly. A child collection therefore
//!    evacuates the parent's continuation tree and rewrites the cell in place; on
//!    child teardown the (GC-current) pointer is read back out. The L7 asserts
//!    stay UNCHANGED and still fire for the illegal case — a plain run entry
//!    (`run`/`run_pure`/`run_fragment`/`*_and_bind`) started while a continuation
//!    is stowed and UNREGISTERED. The child entries
//!    ([`JitEffectMachine::run_child_fragment`] and its pure sibling) are the
//!    only sanctioned way to run while suspended: they register the root, and by
//!    moving the pointer into the cell they leave `suspended_continuation` reading
//!    `None` for the child's duration, so the child fragment drives through the
//!    plain entries whose asserts then pass naturally.
//!
//! 2. **Reclaim/cursor nesting.** A child turn's [`RegistryGuard::drop`] reclaims
//!    the session heap buffer + high-water cursor into `self.session` (buffer may
//!    have been swapped/doubled by a child GC). The `NestedChildGuard` drops
//!    AFTER the child's `RegistryGuard` (the child guard lives inside
//!    `run_with_entry`; the nested guard is the outer local in
//!    `run_child_fragment`), so it reads the continuation pointer back out of the
//!    stowed cell AFTER the reclaim — observing the POST-child heap. The pointer
//!    stays valid across the reclaim because moving a `Vec<u64>` moves its 24-byte
//!    header, not its heap data (the address the pointer targets is stable). When
//!    the parent later resumes, `install_registries` re-installs that same buffer
//!    and the continuation pointer resolves correctly.
//!
//! `nested_child_depth` counts children currently running against the parent; a
//! parent resume is rejected while it is > 0 (sequential-isolated: exactly one
//! computation on the heap at a time). Module accretion (a child's
//! `add_function`) is inert for the parent — it mints a fresh `FuncId` and does
//! not touch the stowed continuation.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub use cranelift_module::FuncId;
use tidepool_effect::{DispatchEffect, EffectContext, EffectError};
use tidepool_eval::value::Value;
use tidepool_repr::{CoreExpr, DataConTable};

use crate::context::VMContext;
use crate::effect_machine::{CompiledEffectMachine, ConTags};
use crate::heap_bridge;
use crate::machine_state::{machine_state, MachineState};
use crate::nursery::Nursery;
use crate::pipeline::CodegenPipeline;
use crate::yield_type::Yield;

/// Error type for JIT compilation/execution failures.
#[derive(Debug, thiserror::Error)]
pub enum JitError {
    #[error("JIT compilation error: {0}")]
    Compilation(#[from] crate::emit::EmitError),
    #[error("pipeline error: {0}")]
    Pipeline(#[from] crate::pipeline::PipelineError),
    #[error("missing freer-simple constructor '{0}' in DataConTable")]
    MissingConTags(&'static str),
    #[error("effect dispatch error: {0}")]
    Effect(#[from] EffectError),
    #[error("yield error: {0}")]
    Yield(#[from] crate::yield_type::YieldError),
    #[error("heap bridge error: {0}")]
    HeapBridge(#[from] crate::heap_bridge::BridgeError),
    // Transparent: this variant wraps signals from EVERY protected JIT call
    // site (step/resume/apply/bridge), so a site-specific prefix here would
    // lie about the phase (it used to say "during heap bridge" everywhere).
    #[error(transparent)]
    Signal(#[from] crate::signal_safety::SignalError),
    #[error("Effect handler response too large ({nodes} value nodes, max {limit}). Narrow your query to return fewer results.")]
    EffectResponseTooLarge { nodes: usize, limit: usize },
    #[error("VarId collision at load: {0}. This indicates a Haskell-side VarId-scheme regression; set TIDEPOOL_VARID_CHECK=0 only to bypass for bisection.")]
    VarIdCollision(#[from] tidepool_repr::VarIdCollision),
}

/// A pending first-cause `RuntimeError` surfaces as a yield error — the shape
/// `host_fns::surface_error` resolves to at `Result<_, JitError>` boundaries.
impl From<crate::host_fns::RuntimeError> for JitError {
    fn from(err: crate::host_fns::RuntimeError) -> Self {
        JitError::Yield(err.into())
    }
}

/// Kill-switch for the load-time duplicate-VarId check (#313 defense).
/// Default ON; `TIDEPOOL_VARID_CHECK=0` disables it (bisection escape hatch).
fn varid_check_enabled() -> bool {
    std::env::var("TIDEPOOL_VARID_CHECK").map_or(true, |v| v != "0")
}

/// A read-only snapshot of one machine's heap/GC counters
/// ([`JitEffectMachine::heap_stats`]) — plain numbers, no GC/rooting
/// internals exposed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeapStats {
    /// Total capacity of the machine's nursery, in bytes.
    pub nursery_bytes: usize,
    /// The session heap's bump high-water mark, in bytes (0 for a one-shot
    /// machine, or a session machine that hasn't run a turn yet).
    pub live_bytes: usize,
    /// Number of collections this machine has run ([`MachineState::gc_generation`]).
    pub gc_count: u64,
}

/// High-level JIT effect machine.
///
/// Compiles a `CoreExpr` (Haskell effect program) into native code via Cranelift
/// and runs it as a coroutine: the machine yields effect requests, the caller
/// dispatches them through an HList of [`EffectHandler`]s, and resumes with responses.
///
/// ```no_run
/// # use tidepool_codegen::jit_machine::JitEffectMachine;
/// # use tidepool_repr::{CoreExpr, CoreFrame, DataConTable, RecursiveTree, Literal};
/// # let expr: CoreExpr = RecursiveTree { nodes: vec![CoreFrame::Lit(Literal::LitInt(42))] };
/// # let table = DataConTable::new();
/// let mut vm = JitEffectMachine::compile(&expr, &table, 1 << 20)?;
/// let result = vm.run_pure()?;
/// # Ok::<(), tidepool_codegen::jit_machine::JitError>(())
/// ```
///
/// Owns the compiled code, nursery (GC heap), and freer-simple constructor tags.
/// The nursery size (in bytes) controls how much heap is available before GC triggers.
///
/// [`EffectHandler`]: tidepool_effect::EffectHandler
pub struct JitEffectMachine {
    pipeline: CodegenPipeline,
    nursery: Nursery,
    tags: Result<ConTags, &'static str>,
    func_id: FuncId,
    /// aeson-`Value` constructor ids for the `JsonDecode` primop, resolved once
    /// at compile from the `DataConTable` and installed into a host-fn
    /// thread-local at each run entry. `None` if the closure isn't in scope.
    json_con_ids: Option<tidepool_eval::json::JsonConIds>,
    /// `Either`/`I#`/`Text` constructor ids for the `ParseISO8601` primop's host
    /// fn — resolved at compile and installed into the machine state at each run
    /// entry. `None` if those constructors aren't in scope.
    time_con_ids: Option<tidepool_eval::time::TimeConIds>,
    /// External cancellation flag. The JIT installs a thread-local clone of this
    /// `Arc` via `set_cancel_flag` before entering compiled code; the next
    /// GC safepoint observes the flag and aborts execution with
    /// `YieldError::Cancelled` if it has been set. See [`Self::cancel_handle`].
    cancel_flag: Arc<AtomicBool>,
    /// Session state for GHCi-style persistent machines (Wave 1.A).
    /// `None` for one-shot machines created by [`Self::compile`].
    session: Option<SessionState>,
    /// Per-machine ambient state (cancel flag, JSON con ids, stack-map
    /// registry, call depth, diagnostics). Reached at run time via
    /// `(*vmctx).machine_state`, pointed at this field by
    /// `install_registries`/the run entries. Leaves 2 and 3 add more fields.
    machine_state: MachineState,
    /// E2 threadless suspension: the freer-simple continuation heap pointer of a
    /// turn that suspended at the ask boundary (`run_suspendable` →
    /// `SuspendableOutcome::Suspended`), waiting for `resume_suspended`. `None`
    /// for a running or completed machine. The pointer is into this machine's
    /// retained session heap.
    ///
    /// SEGMENT 40 — the safety argument for this pointer changed. It used to be
    /// safe because "no GC runs on a suspended machine" (a temporal argument the
    /// L7 asserts enforced). It is now safe because, while nested CHILD runs
    /// execute against the suspended parent, the continuation is a REGISTERED GC
    /// ROOT: [`Self::enter_nested_child`] copies this pointer into
    /// `stowed_root_cell` and registers that heap-stable cell in the machine's
    /// `stowed_roots` set, so any child collection evacuates the continuation
    /// tree and rewrites the cell in place; on child teardown the (GC-current)
    /// pointer is read back out. `resume_suspended` still re-roots via
    /// `materialize_response_and_resume` for its own answer materialization.
    suspended_continuation: Option<*mut u8>,
    /// Heap-stable cell holding the stowed continuation pointer WHILE a nested
    /// child is running (segment 40). A `Box` (not the `suspended_continuation`
    /// field directly) because the machine itself moves between threads under
    /// the stow-XOR-run discipline: the `Box`'s POINTEE address is a stable heap
    /// allocation that does NOT move with the struct, so the `stowed_roots`
    /// registration (the cell's address) stays valid across the move — exactly
    /// the `OldSpace` slots pattern. `None` unless a child is mid-run.
    stowed_root_cell: Option<Box<*mut u8>>,
    /// Number of nested child runs currently executing against this suspended
    /// parent (segment 40). Zero when idle, suspended-but-no-child, or running
    /// its own turn. A parent resume is rejected while this is > 0 (exactly one
    /// computation on the heap at a time — sequential-isolated). Incremented by
    /// [`Self::enter_nested_child`], decremented on guard drop; the stowed root
    /// is registered on 0→1 and deregistered on 1→0.
    nested_child_depth: usize,
    /// W1b-redux: a value-plane bind whose fragment ran through the SUSPENDABLE
    /// path (`run_fragment_suspendable_binding`/`resume_suspended_binding`) tenures
    /// its `Done` result into old-space and stashes the persistent [`RootSlot`]
    /// here, for the caller to read out AFTER the machine moves back off the eval
    /// thread. Unlike the repl's `run_fragment_and_bind` (which returns the slot
    /// directly on its pinned thread), the harness runs a bind on a scoped eval
    /// thread and a `RootSlot` (`*mut *mut u8`) is `!Send`, so it cannot cross the
    /// scope boundary as a bare value — it rides home INSIDE the machine (already
    /// `Send` under stowed-XOR-running, same as `suspended_continuation`). `None`
    /// except in the window between a bind fragment completing and the caller
    /// taking it via [`Self::take_last_bound_root`]. A fork bind lands here on the
    /// eventual `resume`, not the initial (suspending) run.
    last_bound_root: Option<crate::old_space::RootSlot>,
    /// W4 finalize-by-reference: the persistent root slot of a suspended
    /// `finalize @T closure`'s finalized VALUE (field 1 of the request Con),
    /// tenured at suspend time by [`Self::tenure_finalized_payload`]. `Some`
    /// only while suspended on a closure-valued finalize; read out by
    /// [`Self::take_finalized_root`] when the harness applies the closure by
    /// reference. Rides inside the machine (already `Send` under stow-XOR-run),
    /// like `last_bound_root`, because a `RootSlot` (`*mut *mut u8`) is `!Send`
    /// and cannot cross the eval-thread scope boundary as a bare value.
    suspended_finalized_root: Option<crate::old_space::RootSlot>,
}

// SAFETY: a `JitEffectMachine` is only ever touched by ONE thread at a time —
// it is either stowed as data (E2 suspension) or running on exactly one eval
// thread, never both. This mirrors the existing `unsafe impl Send` on
// `CompiledEffectMachine`/`MachineState`/`GcState`: the JIT executable mappings
// and heap buffers are process-global address space, valid on any thread, and
// the raw `suspended_continuation` pointer is a heap offset into an owned
// buffer that moves with the machine. Concurrent access is prevented by the
// SessionEngine registry (a machine is stowed XOR running), so sending
// ownership across the suspend/resume thread boundary is sound.
unsafe impl Send for JitEffectMachine {}

/// External handle for cancelling a running `JitEffectMachine`.
///
/// `CancelHandle` is `Send + Sync + Clone`, so callers can hand clones to
/// watchdog threads. Cancellation is observed at the next GC safepoint
/// (heap check), which fires on essentially every non-trivial allocation in
/// Haskell code. The running program unwinds via the normal error path with
/// `JitError::Yield(YieldError::Cancelled)`.
///
/// The flag is per-`JitEffectMachine`, not per-run: call [`Self::reset`]
/// between runs if you intend to reuse the machine after a cancellation.
#[derive(Clone, Debug)]
pub struct CancelHandle(Arc<AtomicBool>);

impl CancelHandle {
    /// Request cancellation of the associated `JitEffectMachine`. The running
    /// program (if any) will abort at its next GC safepoint with
    /// `YieldError::Cancelled`.
    pub fn cancel(&self) {
        // SeqCst is overkill for correctness here (the JIT thread's relaxed
        // load will observe the store eventually), but this is not a hot path
        // — it is called once from a watchdog — so we prefer the stronger
        // ordering for debuggability.
        self.0.store(true, Ordering::SeqCst);
    }

    /// Returns `true` if cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// Clear a previous cancellation request. Call this between runs if the
    /// same `JitEffectMachine` is reused after a cancelled run.
    pub fn reset(&self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Session-level heap + cursor retained across runs (Wave 1.A).
///
/// `heap` is `None` until the first GC fires and migrates the live set off
/// the machine's `Nursery` into a `Vec<u64>` owned here — `Vec<u64>`, not
/// `Vec<u8>` (L8, repo-review-2026-07-06/01-gc-memory-safety.md): heap
/// objects are read/written assuming 8-byte alignment, which `Vec<u64>`
/// guarantees structurally (unlike `Vec<u8>`, whose element alignment is 1
/// — any alignment it happens to have is an allocator implementation
/// detail). `cursor` is the bump high-water mark (bytes from the start of
/// `heap`, or from `nursery.start()` when `heap` is None) at the end of the
/// last run — the next run resumes allocation from there.
struct SessionState {
    heap: Option<Vec<u64>>,
    cursor: usize,
    // Wave 1.A (Worker-Tenure): populated at bind time; read by the four
    // *_and_bind* run entries to tenure NF values into stable old-space slots.
    old_space: crate::old_space::OldSpace,
}

/// Ensures thread-local JIT registries are cleaned up even on early error return.
///
/// For one-shot machines (`is_session = false`), Drop behaves as before.
/// For session machines (`is_session = true`):
///   - `reclaim` is set by `arm_reclaim` after the vmctx is at its final
///     location; Drop reads `alloc_ptr` from the vmctx and calls
///     `reclaim_session_heap` to move `active_buffer` back onto the machine
///     BEFORE `clear_run_scratch` takes the GcState.
///   - `clear_run_scratch` (not `clear_gc_state`) runs per-run; it drops
///     only the GcState shell + RUST_ROOTS, leaving PERSISTENT_ROOTS alone.
pub(crate) struct RegistryGuard {
    is_session: bool,
    /// Raw pointers captured by `arm_reclaim`. Both point into the same
    /// stack frame as this guard (run / run_pure), which cannot have
    /// returned by the time Drop runs. VMContext has no custom Drop, so its
    /// bytes are valid on the stack even after the value is logically dropped.
    reclaim: Option<ReclaimTargets>,
    /// Points at the owning `JitEffectMachine::machine_state`, set by
    /// `install_registries`. Outlives this guard (same call frame).
    machine_state: *mut MachineState,
    /// The thread's `CURRENT_MACHINE` value before `install_registries`
    /// installed `machine_state` (null unless runs nest) — restored on drop.
    prev_machine: *mut MachineState,
}

/// The two raw pointers `arm_reclaim` captures for the Drop-time heap reclaim.
/// A named struct (not a bare tuple) so the `session_slot` and `vmctx` fields —
/// both raw pointers — cannot be silently transposed at a call site.
struct ReclaimTargets {
    /// Points to `JitEffectMachine::session` (same call frame as the guard).
    session_slot: *mut Option<SessionState>,
    /// Points to the VMContext used for this run (same call frame; no Drop).
    vmctx: *const crate::context::VMContext,
}

impl RegistryGuard {
    /// Arm the reclaim step for session machines. Called after the vmctx is
    /// at its final stable location (local `vmctx` in run_pure, inside
    /// `CompiledEffectMachine` in run).
    ///
    /// # Safety
    /// - `session` must point to `JitEffectMachine::session` and remain
    ///   valid until this guard drops (it's in the same call frame).
    /// - `vmctx` must point to the VMContext used for this run and remain
    ///   readable until Drop (no custom Drop on VMContext, so the stack
    ///   bytes persist until the enclosing frame returns).
    unsafe fn arm_reclaim(
        &mut self,
        session: *mut Option<SessionState>,
        vmctx: *const crate::context::VMContext,
    ) {
        if self.is_session {
            self.reclaim = Some(ReclaimTargets {
                session_slot: session,
                vmctx,
            });
        }
    }
}

impl Drop for RegistryGuard {
    fn drop(&mut self) {
        // Reclaim the live heap buffer back onto the machine BEFORE
        // clear_run_scratch takes GcState (which would free active_buffer).
        if let Some(ReclaimTargets {
            session_slot,
            vmctx,
        }) = self.reclaim
        {
            // SAFETY: vmctx points into the enclosing run/run_pure stack
            // frame which is still live. VMContext has no custom Drop so its
            // bytes are intact even after the value is logically dropped.
            // session_slot points to JitEffectMachine::session in the same frame.
            unsafe {
                let ap = (*vmctx).alloc_ptr;
                let (buf, cur) = (*self.machine_state).reclaim_session_heap(ap);
                if let Some(s) = (*session_slot).as_mut() {
                    s.heap = buf;
                    s.cursor = cur;
                }
            }
        }
        // SAFETY: machine_state was set by install_registries and outlives
        // this guard (points at the owning JitEffectMachine's field). Clean
        // the per-run cells (including the GC-cluster's clear_run_scratch)
        // directly through the machine (not the ambient free-fn shims)
        // BEFORE restoring CURRENT_MACHINE below — otherwise the free fns
        // would see a cleared/stale current-machine pointer.
        unsafe {
            (*self.machine_state).clear_run_scratch();
            (*self.machine_state).clear_stack_map_registry();
            (*self.machine_state).clear_cancel_flag();
            let _ = (*self.machine_state).take_runtime_error();
            let _ = (*self.machine_state).drain_diagnostics();
            (*self.machine_state).clear_parked_streams();
            (*self.machine_state).reset_call_depth();
        }
        crate::debug::clear_lambda_registry();
        crate::host_fns::set_exec_context("");
        crate::machine_state::restore_current_machine(self.prev_machine);
    }
}

/// The compiled artifacts produced by [`JitEffectMachine::compile_inner`],
/// shared by the one-shot (`compile`) and session (`compile_session`) ctors.
type CompiledParts = (
    CodegenPipeline,
    Nursery,
    Result<ConTags, &'static str>,
    FuncId,
    Option<tidepool_eval::json::JsonConIds>,
    Option<tidepool_eval::time::TimeConIds>,
);

impl JitEffectMachine {
    /// Shared compilation body: normalise, emit, finalise.
    fn compile_inner(
        expr: &CoreExpr,
        table: &DataConTable,
        nursery_size: usize,
    ) -> Result<CompiledParts, JitError> {
        crate::debug::init_logging();
        // #313 defense: a duplicate VarId on the top-level Let spine means two
        // distinct top-level bindings silently shadow each other — fail loudly
        // at load instead. Runs on the raw deserialized tree (the wrapAllBinds
        // Let-nest), before normalize/datacon wrapping reshape it.
        if varid_check_enabled() {
            tidepool_repr::check_toplevel_varids(expr)?;
        }
        let expr = tidepool_repr::normalize(expr, table);
        // The wrapper manifest is dropped here: `lower_jump_crosses_lam` below
        // rebuilds the tree, so its node indices would not survive. The manifest
        // exists for the session re-entry path (`add_function`), which has prior
        // fragments to share constructor closures with; a one-shot compile has
        // none.
        let expr = crate::datacon_env::wrap_with_datacon_env(expr, table).expr;
        // Defensive precondition restore: the real Haskell pipeline never emits
        // a Jump crossing a Lam boundary (Translate.hs's `jumpCrossesLam` rewrites
        // it first), but hand-built/synthetic CoreExpr producers can. Re-check
        // after normalize/datacon-env wrapping so whatever final shape reaches
        // emission satisfies codegen's join-registration invariant.
        let expr = crate::lower::lower_jump_crosses_lam(&expr);
        let mut pipeline = CodegenPipeline::new(&crate::host_fns::host_fn_symbols())?;
        // Give data-case dispatch runtime tolerance for bare Lit scrutinees of
        // boxed-literal wrapper constructors (e.g. a Rust-materialized aeson
        // `Number`'s LitDouble reaching `case x of { D# ds -> .. }`).
        pipeline.lit_wrappers = crate::emit::LitWrapperIds::from_table(table);
        // No session bindings on initial compile, so the external env is empty.
        let func_id = crate::emit::expr::compile_expr(
            &mut pipeline,
            &expr,
            "main",
            &crate::emit::ExternalEnv::new(),
        )
        .map_err(JitError::Compilation)?;
        pipeline.finalize()?;
        let tags = ConTags::from_table(table).map_err(|kind| kind.name());
        let nursery = Nursery::new(nursery_size);
        // Cache the aeson-`Value` constructor ids for the `JsonDecode` primop's
        // host fn, and the `Either`/`I#`/`Text` ids for `ParseISO8601` (both
        // installed into the machine state before each run).
        let json_con_ids = tidepool_eval::json::JsonConIds::from_table(table);
        let time_con_ids = tidepool_eval::time::TimeConIds::from_table(table);
        Ok((pipeline, nursery, tags, func_id, json_con_ids, time_con_ids))
    }

    /// Compile a CoreExpr for one-shot JIT execution.
    ///
    /// The returned machine has no session state: the heap lives in the
    /// machine's `Nursery` and is discarded after each run.
    pub fn compile(
        expr: &CoreExpr,
        table: &DataConTable,
        nursery_size: usize,
    ) -> Result<Self, JitError> {
        let (pipeline, nursery, tags, func_id, json_con_ids, time_con_ids) =
            Self::compile_inner(expr, table, nursery_size)?;
        Ok(Self {
            pipeline,
            nursery,
            tags,
            func_id,
            json_con_ids,
            time_con_ids,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            session: None,
            machine_state: MachineState::new(),
            suspended_continuation: None,
            stowed_root_cell: None,
            nested_child_depth: 0,
            last_bound_root: None,
            suspended_finalized_root: None,
        })
    }

    /// Compile a CoreExpr for GHCi-style session execution.
    ///
    /// The returned machine retains its heap across multiple runs: the live
    /// heap after the first GC is moved into `SessionState::heap` and
    /// re-installed on every subsequent `run`/`run_pure` call. Persistent
    /// GC roots (registered via [`Self::register_persistent_root`]) survive
    /// across runs and are cleared only when the machine is dropped.
    pub fn compile_session(
        expr: &CoreExpr,
        table: &DataConTable,
        nursery_size: usize,
    ) -> Result<Self, JitError> {
        let (pipeline, nursery, tags, func_id, json_con_ids, time_con_ids) =
            Self::compile_inner(expr, table, nursery_size)?;
        Ok(Self {
            pipeline,
            nursery,
            tags,
            func_id,
            json_con_ids,
            time_con_ids,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            session: Some(SessionState {
                heap: None,
                cursor: 0,
                old_space: crate::old_space::OldSpace::new(),
            }),
            machine_state: MachineState::new(),
            suspended_continuation: None,
            stowed_root_cell: None,
            nested_child_depth: 0,
            last_bound_root: None,
            suspended_finalized_root: None,
        })
    }

    /// Obtain a clone-able, thread-safe handle for requesting cancellation of
    /// this machine's next (or in-flight) run. The handle remains valid for
    /// the lifetime of the machine; multiple handles may be held concurrently.
    pub fn cancel_handle(&self) -> CancelHandle {
        CancelHandle(self.cancel_flag.clone())
    }

    /// Drain this machine's accumulated diagnostics. The machine-scoped
    /// sibling of the ambient `host_fns::drain_diagnostics` free-fn shim
    /// (per #340).
    pub fn drain_diagnostics(&self) -> Vec<String> {
        self.machine_state.drain_diagnostics()
    }

    /// Install per-run thread-local registries and return a drop guard.
    ///
    /// For session machines: re-points the GC state at the retained heap
    /// buffer (if a GC has already run) OR at `nursery.start()` (first run
    /// only). For one-shot machines: always points at `nursery.start()`.
    pub(crate) fn install_registries(&mut self) -> RegistryGuard {
        crate::debug::set_lambda_registry(self.pipeline.build_lambda_registry());
        self.machine_state
            .set_stack_map_registry(&self.pipeline.stack_maps);
        match &mut self.session {
            Some(s) => match s.heap.take() {
                Some(buf) => self.machine_state.install_session_buffer(buf),
                None => self
                    .machine_state
                    .set_gc_state(self.nursery.start() as *mut u8, self.nursery.size()),
            },
            None => self
                .machine_state
                .set_gc_state(self.nursery.start() as *mut u8, self.nursery.size()),
        }
        self.machine_state.set_cancel_flag(self.cancel_flag.clone());
        // Make the aeson-`Value` constructor ids (JsonDecode) and the
        // Either/I#/Text ids (ParseISO8601) visible to those primops' host fns
        // for the duration of this run.
        self.machine_state.set_json_con_ids(self.json_con_ids);
        self.machine_state.set_time_con_ids(self.time_con_ids);
        let machine_state_ptr = &mut self.machine_state as *mut MachineState;
        // Install this machine as the thread's reach target for vmctx-less
        // host fns and the external ambient shims; RegistryGuard::drop
        // restores whatever was installed before (null unless runs nest).
        let prev_machine = crate::machine_state::install_current_machine(machine_state_ptr);
        RegistryGuard {
            is_session: self.session.is_some(),
            reclaim: None,
            machine_state: machine_state_ptr,
            prev_machine,
        }
    }

    /// Build a `VMContext` for a session run, re-pointing alloc_ptr at the
    /// persistent cursor (component F).
    ///
    /// Reads the active GC region from `GC_STATE` (installed by
    /// `install_registries` immediately before this call) and sets
    /// `alloc_ptr = start + cursor` so the run resumes from the last
    /// run's high-water mark rather than overwriting live data.
    ///
    /// # Panics
    /// Panics if called without GC state installed or on a non-session machine.
    fn make_session_vmctx(&self) -> crate::context::VMContext {
        let (start, size) = self
            .machine_state
            .gc_active_range()
            .expect("GC state must be installed before make_session_vmctx");
        let cursor = self
            .session
            .as_ref()
            .expect("make_session_vmctx called on non-session machine")
            .cursor;
        // SAFETY: start..start+size is the session heap installed by
        // install_registries. cursor <= size is maintained by reclaim_session_heap.
        let mut vmctx = unsafe {
            crate::context::VMContext::new(start, start.add(size), crate::host_fns::gc_trigger)
        };
        // SAFETY: cursor <= size guaranteed by the reclaim invariant.
        vmctx.alloc_ptr = unsafe { start.add(cursor) };
        vmctx
    }

    /// Run to completion, dispatching effects through the handler HList.
    pub fn run<U, H: DispatchEffect<U>>(
        &mut self,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
    ) -> Result<Value, JitError> {
        let func_id = self.func_id;
        self.run_with_entry(func_id, table, handlers, user)
    }

    /// Shared effectful-run body, parametrized by the entry `func_id`.
    ///
    /// [`Self::run`] passes the machine's original entry; [`Self::run_fragment`]
    /// passes an [`Self::add_function`]-minted fragment id. The lifecycle is
    /// identical either way (session vmctx, reclaim arming, effect loop), so the
    /// one-shot path is byte-identical to the pre-refactor `run`.
    fn run_with_entry<U, H: DispatchEffect<U>>(
        &mut self,
        func_id: FuncId,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
    ) -> Result<Value, JitError> {
        // L7 (repo-review-2026-07-06/01-gc-memory-safety.md): starting a new
        // turn while a prior one is still parked at `resume_suspended` isn't
        // a GC-rooted invariant anything else enforces — closing over it
        // here instead of relying on tidepool-repl's external discipline.
        // Shared by `run` and `run_fragment`.
        assert!(
            self.suspended_continuation.is_none(),
            "run/run_fragment called while a continuation is suspended — \
             resume_suspended it first"
        );
        let tags = self.tags.map_err(JitError::MissingConTags)?;

        // Ensure signal handlers + this thread's alternate stack are installed:
        // library embedders (compile_and_run*) don't call install() themselves,
        // and without it a JIT fault kills the whole process instead of
        // surfacing a clean YieldError. Idempotent per thread.
        crate::signal_safety::install();

        // Install registries
        let mut _guard = self.install_registries();

        // SAFETY: get_function_ptr returns a finalized JIT code pointer. Transmuting to the
        // expected calling convention (vmctx -> result) is correct per our compilation contract.
        let func_ptr: unsafe extern "C" fn(*mut VMContext) -> *mut u8 =
            unsafe { std::mem::transmute(self.pipeline.get_function_ptr(func_id)) };
        let vmctx = if self.session.is_some() {
            self.make_session_vmctx()
        } else {
            self.nursery.make_vmctx(crate::host_fns::gc_trigger)
        };

        let mut machine = CompiledEffectMachine::new(func_ptr, vmctx, tags);
        // SAFETY: machine_state outlives this run (owned by self); machine's
        // vmctx is stable for the run's duration.
        machine.vmctx_mut().machine_state = &mut self.machine_state as *mut MachineState;
        // Arm reclaim so Drop can recover active_buffer → session.heap.
        // SAFETY: machine.vmctx_mut() points into `machine` on this stack frame;
        // CompiledEffectMachine has no custom Drop so the bytes are valid when
        // _guard drops (machine drops first but the stack frame is still live).
        unsafe {
            _guard.arm_reclaim(&mut self.session as *mut _, machine.vmctx_mut() as *const _);
        }
        let done_ptr = drive_to_done(
            &mut machine,
            &self.cancel_flag,
            table,
            handlers,
            user,
            "stepping main function",
            "",
        )?;
        // SAFETY: done_ptr is a valid heap pointer returned by the JIT.
        // vmctx_ptr is valid for forcing thunks. Signal protection guards
        // against crashes.
        let bridge_res = unsafe {
            let vmctx_ptr = machine.vmctx_mut() as *mut VMContext;
            crate::signal_safety::with_signal_protection(|| {
                heap_bridge::heap_to_value_forcing(done_ptr, vmctx_ptr)
            })
        }
        .map_err(JitError::Signal)?;
        // A cancel observed during forcing (`gc_trigger`) records the first
        // cause; the bridge outcome — even a successful bridge of a poison
        // value — is only its symptom.
        crate::host_fns::surface_error(bridge_res.map_err(JitError::HeapBridge))
    }

    // ----------------------------------------------------------------------
    // E2 — threadless suspension at the ask boundary.
    // ----------------------------------------------------------------------

    /// Drive an effectful turn until it COMPLETES or SUSPENDS at `suspend_tag`
    /// (the `Ask` union tag). Additive sibling of [`Self::run`]: a turn that
    /// never reaches `suspend_tag` drives byte-identically — the effect loop's
    /// suspend branch is simply never taken (see [`drive_effect_loop`]).
    ///
    /// On suspension the machine's heap is retained through the session
    /// machinery (`RegistryGuard::drop` → `reclaim_session_heap`) and the
    /// continuation is stowed inside `self`; the whole `JitEffectMachine` can
    /// then be moved off this thread and parked as data. Call
    /// [`Self::resume_suspended`] with the answer to continue on ANY thread.
    ///
    /// # Panics
    /// Panics on a non-session machine — heap retention across the suspension
    /// requires [`Self::compile_session`].
    pub fn run_suspendable<U, H: DispatchEffect<U>>(
        &mut self,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
        suspend_tag: u64,
    ) -> Result<SuspendableOutcome, JitError> {
        let func_id = self.func_id;
        self.run_suspendable_with_entry(func_id, table, handlers, user, suspend_tag, None)
    }

    /// Suspend-capable sibling of [`Self::run_fragment`]: drive an
    /// [`Self::add_function`]-minted fragment through the same threadless
    /// suspend path [`Self::run_suspendable`] uses for the machine's original
    /// entry. A fragment that reaches `suspend_tag` (an `Ask`) mid-computation
    /// stows its continuation on `self` exactly as the entry path does; the
    /// binding it was computing lands on [`Self::resume_suspended`] to
    /// completion. This is the composition of the fragment plane (C2 session
    /// re-entry) with E2 threadless suspension — same shared
    /// [`drive_effect_loop`], only the entry `func_id` differs.
    ///
    /// # Panics
    /// Panics on a non-session machine — heap retention across the suspension
    /// requires [`Self::compile_session`].
    pub fn run_fragment_suspendable<U, H: DispatchEffect<U>>(
        &mut self,
        func_id: FuncId,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
        suspend_tag: u64,
    ) -> Result<SuspendableOutcome, JitError> {
        self.run_suspendable_with_entry(func_id, table, handlers, user, suspend_tag, None)
    }

    /// Value-plane BIND sibling of [`Self::run_fragment_suspendable`]: drive a
    /// bind fragment (`x <- e`) through the same threadless suspend path, and — on
    /// `Done` — tenure the result into old-space, stashing its [`RootSlot`] on the
    /// machine (read via [`Self::take_last_bound_root`] after the machine moves off
    /// the eval thread). `forced` deep-forces the result to NF first (Tier0 data)
    /// vs tenuring a Tier1 closure as-is. A fork bind SUSPENDS here (no tenure yet);
    /// its value is bound on the eventual [`Self::resume_suspended_binding`].
    pub fn run_fragment_suspendable_binding<U, H: DispatchEffect<U>>(
        &mut self,
        func_id: FuncId,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
        suspend_tag: u64,
        forced: bool,
    ) -> Result<SuspendableOutcome, JitError> {
        self.run_suspendable_with_entry(func_id, table, handlers, user, suspend_tag, Some(forced))
    }

    /// Shared suspend-capable run body, parametrized by the entry `func_id`.
    /// [`Self::run_suspendable`] passes the machine's original entry;
    /// [`Self::run_fragment_suspendable`] passes an [`Self::add_function`]-minted
    /// fragment id. The lifecycle is identical either way (session vmctx, reclaim
    /// arming, suspend-capable effect loop), so the entry path stays
    /// byte-identical to the pre-refactor `run_suspendable`.
    fn run_suspendable_with_entry<U, H: DispatchEffect<U>>(
        &mut self,
        func_id: FuncId,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
        suspend_tag: u64,
        bind_forced: Option<bool>,
    ) -> Result<SuspendableOutcome, JitError> {
        assert!(
            self.session.is_some(),
            "run_suspendable requires a session machine (compile_session)"
        );
        // L7: see run_with_entry's doc.
        assert!(
            self.suspended_continuation.is_none(),
            "run_suspendable called while a continuation is already suspended — \
             resume_suspended it first"
        );
        let tags = self.tags.map_err(JitError::MissingConTags)?;
        crate::signal_safety::install();
        let mut _guard = self.install_registries();
        // SAFETY: finalized JIT code pointer; calling convention per contract.
        let func_ptr: unsafe extern "C" fn(*mut VMContext) -> *mut u8 =
            unsafe { std::mem::transmute(self.pipeline.get_function_ptr(func_id)) };
        let vmctx = self.make_session_vmctx();
        let mut machine = CompiledEffectMachine::new(func_ptr, vmctx, tags);
        // SAFETY: machine_state outlives this run (owned by self).
        machine.vmctx_mut().machine_state = &mut self.machine_state as *mut MachineState;
        // Reclaim is armed LAST (after finish_suspendable), NOT here: a bind finish
        // tenures into self.session, and arm_reclaim's stored *mut self.session
        // would alias it (mirrors run_fragment_and_bind's arm-last ordering). Safe
        // for the non-bind case too — nothing touches self.session before the arm,
        // and the arm runs unconditionally (even on a run error) so the guard still
        // reclaims the session buffer on drop.
        let yield_result = initial_step(&mut machine, "stepping main function");
        let finished = match drive_effect_loop(
            &mut machine,
            &self.cancel_flag,
            table,
            handlers,
            user,
            "",
            Some(suspend_tag),
            yield_result,
        ) {
            Ok(outcome) => self.finish_suspendable(&mut machine, outcome, bind_forced),
            Err(e) => Err(e),
        };
        // SAFETY: machine.vmctx_mut() points into `machine` on this frame;
        // CompiledEffectMachine has no custom Drop so the bytes are valid when
        // _guard drops (machine drops first but the frame is still live). The
        // guard's reclaim reads the post-run buffer/cursor back into self.session.
        unsafe {
            _guard.arm_reclaim(&mut self.session as *mut _, machine.vmctx_mut() as *const _);
        }
        finished
    }

    /// Re-enter a turn suspended by [`Self::run_suspendable`], feeding the
    /// (already schema-validated, bridged) answer — or an abort — into the
    /// stowed ask and driving to the next suspension or completion.
    ///
    /// Runs on ANY thread: [`Self::install_registries`] re-installs this
    /// machine's per-thread reach (`CURRENT_MACHINE`, stack-map/lambda
    /// registry, cancel flag) and re-points the GC state at the RETAINED
    /// session heap. It must NOT reset the nursery — the session heap-retention
    /// path (heap `Some` → `install_session_buffer`, or `None` → nursery at the
    /// preserved cursor) preserves the mid-ask heap; a nursery reset would
    /// silently discard it. That is edit site (a).
    ///
    /// Errors if the machine is not currently suspended.
    pub fn resume_suspended<U, H: DispatchEffect<U>>(
        &mut self,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
        suspend_tag: u64,
        input: ResumeInput,
    ) -> Result<SuspendableOutcome, JitError> {
        self.resume_suspended_inner(table, handlers, user, suspend_tag, input, None)
    }

    /// Value-plane BIND sibling of [`Self::resume_suspended`]: re-enter a suspended
    /// bind turn (`x <- e` that stowed at a fork) and, on `Done`, tenure the bound
    /// result — stashing its [`RootSlot`] on the machine
    /// ([`Self::take_last_bound_root`]). `forced` deep-forces to NF (Tier0) vs
    /// tenuring a Tier1 closure as-is.
    pub fn resume_suspended_binding<U, H: DispatchEffect<U>>(
        &mut self,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
        suspend_tag: u64,
        input: ResumeInput,
        forced: bool,
    ) -> Result<SuspendableOutcome, JitError> {
        self.resume_suspended_inner(table, handlers, user, suspend_tag, input, Some(forced))
    }

    /// Shared body of [`Self::resume_suspended`] /
    /// [`Self::resume_suspended_binding`], parametrized by `bind_forced` (`None` →
    /// plain resume; `Some(forced)` → tenure the completed result as a value-plane
    /// bind).
    fn resume_suspended_inner<U, H: DispatchEffect<U>>(
        &mut self,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
        suspend_tag: u64,
        input: ResumeInput,
        bind_forced: Option<bool>,
    ) -> Result<SuspendableOutcome, JitError> {
        // PEEK the continuation — do NOT consume it yet. The A5 NF-force
        // (segment 40) rejects a bottom-bearing answer WITHOUT consuming the
        // continuation, so the caller can retry with a corrected answer; only
        // after the answer is verified NF do we `.take()` (below). A `None`
        // here (not suspended) is the same clean error as before.
        if self.suspended_continuation.is_none() {
            return Err(JitError::Effect(EffectError::Handler(
                "resume_suspended called on a machine that is not suspended".into(),
            )));
        }
        // A5 — NF-force the data-kinded answer BEFORE consuming the
        // continuation. A bottom anywhere in the answer (a residual unforced
        // thunk — an `undefined`/`⊥` the child-answer bridge would have raised,
        // caught here as defense-in-depth) fails the answer as a retryable
        // error and leaves `suspended_continuation` intact. Function-bearing
        // answer types were rejected at extract (segment 10), so every field of
        // a data-kinded answer is walkable by construction; the walk terminates
        // on a visited-set (cyclic data).
        if let ResumeInput::Answer(val) = &input {
            answer_force_nf(val).map_err(|reason| {
                JitError::Effect(EffectError::Handler(format!(
                    "resume answer is not in normal form (bottom in the answer): {reason}"
                )))
            })?;
        }
        // Answer verified NF (or this is an Abort) — NOW consume the
        // continuation. Every early return above left it stowed.
        let continuation = self
            .suspended_continuation
            .take()
            .expect("suspended_continuation present (checked is_some above)");
        let tags = self.tags.map_err(JitError::MissingConTags)?;
        crate::signal_safety::install();
        // Re-points GC state at the retained heap (heap `Some` → session buffer,
        // else nursery at the preserved cursor) — NOT a nursery reset.
        let mut _guard = self.install_registries();
        // SAFETY: finalized JIT code pointer. The entry func is not re-called on
        // resume (the continuation is applied via `machine.resume`), but
        // CompiledEffectMachine needs a func_ptr for its own tail-call resolution.
        let func_ptr: unsafe extern "C" fn(*mut VMContext) -> *mut u8 =
            unsafe { std::mem::transmute(self.pipeline.get_function_ptr(self.func_id)) };
        let vmctx = self.make_session_vmctx();
        let mut machine = CompiledEffectMachine::new(func_ptr, vmctx, tags);
        // SAFETY: machine_state outlives this run (owned by self).
        machine.vmctx_mut().machine_state = &mut self.machine_state as *mut MachineState;
        // Reclaim is armed LAST (after finish_suspendable), NOT here — see
        // run_suspendable_with_entry: a bind finish tenures into self.session and
        // arm_reclaim's *mut self.session would alias it. The abort branch arms
        // explicitly before it returns (it never reaches the tail arm below).

        let answer = match input {
            ResumeInput::Answer(val) => val,
            ResumeInput::Abort(reason) => {
                // Edit site (b): a stowed machine has no thread. We do NOT run
                // the continuation — the ask itself fails, byte-identically to
                // pre-E2's answer-channel abort, which returned
                // `EffectError::Handler("ask aborted by caller: …")` from the
                // dispatcher (no `Cancelled` first cause — that was only the
                // gate/timeout abort). The engine maps this to its terminal
                // error outcome exactly as before. `install_registries` above
                // already installed this machine as `CURRENT_MACHINE`, so any
                // machine-scoped state stays reachable, but this early return
                // surfaces the error directly without touching the first-cause
                // cell.
                // Abort does not run the continuation, so it never reaches the
                // tail arm; arm+drop the guard here to restore the session buffer
                // exactly as the pre-W1b path did (which armed before this branch).
                unsafe {
                    _guard
                        .arm_reclaim(&mut self.session as *mut _, machine.vmctx_mut() as *const _);
                }
                return Err(JitError::Effect(EffectError::Handler(format!(
                    "ask aborted by caller: {reason}"
                ))));
            }
        };

        // Feed the answer as a Complete response through the SAME materialization
        // + resume path the effect loop uses, then continue driving. Capture the
        // result WITHOUT `?` so the tail arm runs on every path (a bind finish
        // tenures into self.session, so the arm must follow finish_suspendable).
        let finished = match materialize_response_and_resume(
            &mut machine,
            continuation,
            tidepool_effect::Response::Complete(answer),
            table,
            suspend_tag,
            "",
        ) {
            Ok(yield_result) => match drive_effect_loop(
                &mut machine,
                &self.cancel_flag,
                table,
                handlers,
                user,
                "",
                Some(suspend_tag),
                yield_result,
            ) {
                Ok(outcome) => self.finish_suspendable(&mut machine, outcome, bind_forced),
                Err(e) => Err(e),
            },
            Err(e) => Err(e),
        };
        // SAFETY: machine.vmctx_mut() points into `machine` on this frame; the
        // guard's reclaim reads the post-run buffer/cursor into self.session.
        unsafe {
            _guard.arm_reclaim(&mut self.session as *mut _, machine.vmctx_mut() as *const _);
        }
        finished
    }

    /// Shared epilogue for the suspendable path: bridge a `Done` pointer to a
    /// `Value` (byte-identical to [`Self::run_with_entry`]'s epilogue), or stow
    /// the continuation on `self` and surface the suspension.
    ///
    /// `bind_forced` distinguishes a plain suspendable turn (`None` — bridge the
    /// `Done` pointer, byte-identical to the pre-W1b epilogue) from a VALUE-PLANE
    /// BIND (`Some(forced)` — tenure the `Done` result into old-space, stash its
    /// [`RootSlot`] on `self.last_bound_root`, and bridge the tenured value). The
    /// bind branch touches `self.session` (via `tenure`), so a bind caller MUST NOT
    /// have armed reclaim before this call (the guard's `*mut self.session` would
    /// alias) — see `run_fragment_and_bind`'s arm-last ordering.
    fn finish_suspendable(
        &mut self,
        machine: &mut CompiledEffectMachine,
        outcome: DriveOutcome,
        bind_forced: Option<bool>,
    ) -> Result<SuspendableOutcome, JitError> {
        match outcome {
            DriveOutcome::Done(done_ptr) => {
                if let Some(forced) = bind_forced {
                    if done_ptr.is_null() {
                        return Err(JitError::Yield(crate::yield_type::YieldError::NullPointer));
                    }
                    // Optionally deep-force to NF before tenuring (Tier0 data);
                    // Tier1 closures tenure as-is (callable code, not data). Mirrors
                    // `run_fragment_and_bind`'s K/E/D epilogue.
                    let nf_ptr = if forced {
                        let nf = unsafe {
                            crate::signal_safety::with_signal_protection(|| {
                                crate::host_fns::deep_force(
                                    machine.vmctx_mut() as *mut VMContext,
                                    done_ptr,
                                )
                            })
                        }
                        .map_err(JitError::Signal)?;
                        if let Some(err) = crate::host_fns::take_runtime_error() {
                            return Err(JitError::Yield(crate::yield_type::YieldError::from(err)));
                        }
                        nf
                    } else {
                        done_ptr
                    };
                    let from = self
                        .machine_state
                        .gc_active_range()
                        .expect("GC state installed for the suspendable bind run");
                    let from_range = (from.0 as *const u8, unsafe {
                        from.0.add(from.1) as *const u8
                    });
                    let vmctx_ptr = machine.vmctx_mut() as *mut VMContext;
                    // SAFETY: nf_ptr is a live heap object in the nursery from-range;
                    // tenure evacuates its closure into old-space and registers the
                    // returned slot as a persistent root valid for the machine's
                    // life. `self.session` is unaliased (reclaim not yet armed).
                    let slot = unsafe {
                        self.session
                            .as_mut()
                            .expect("session machine for a bind tenure")
                            .old_space
                            .tenure(vmctx_ptr, nf_ptr, from_range)
                    };
                    self.last_bound_root = Some(slot);
                    // Bridge the TENURED (rooted, stable) value for the turn's
                    // rendered result. SAFETY: slot.current() is the live old-space
                    // pointer; forcing is a no-op on the already-NF Tier0 case.
                    let bridge_res = unsafe {
                        let live = slot.current();
                        crate::signal_safety::with_signal_protection(|| {
                            heap_bridge::heap_to_value_forcing(live, vmctx_ptr)
                        })
                    }
                    .map_err(JitError::Signal)?;
                    let value =
                        crate::host_fns::surface_error(bridge_res.map_err(JitError::HeapBridge))?;
                    return Ok(SuspendableOutcome::Completed(value));
                }
                // SAFETY: done_ptr is a valid heap pointer returned by the JIT;
                // vmctx is valid for forcing thunks; signal protection guards
                // against crashes.
                let bridge_res = unsafe {
                    let vmctx_ptr = machine.vmctx_mut() as *mut VMContext;
                    crate::signal_safety::with_signal_protection(|| {
                        heap_bridge::heap_to_value_forcing(done_ptr, vmctx_ptr)
                    })
                }
                .map_err(JitError::Signal)?;
                let value =
                    crate::host_fns::surface_error(bridge_res.map_err(JitError::HeapBridge))?;
                Ok(SuspendableOutcome::Completed(value))
            }
            DriveOutcome::Suspended {
                request,
                request_ptr,
                continuation,
            } => {
                // W4 finalize-by-reference: when the bridged request carries a
                // CLOSURE_SENTINEL placeholder, its value field (field 1 of the
                // request Con) is a live closure with no data representation.
                // Tenure it into old-space NOW — while we still hold the run's
                // active GC range and a valid vmctx — so it survives any later
                // child GC as a persistent root, and hand the slot up so the
                // harness can apply it by reference via `run_child`.
                let has_finalized_closure = request_carries_closure_sentinel(&request);
                if has_finalized_closure {
                    let slot = self.tenure_finalized_payload(machine, request_ptr)?;
                    self.suspended_finalized_root = Some(slot);
                }
                self.suspended_continuation = Some(continuation);
                Ok(SuspendableOutcome::Suspended {
                    request,
                    has_finalized_closure,
                })
            }
        }
    }

    /// Tenure the finalized VALUE (field 1) out of a suspended `finalize @T x`
    /// request Con into old-space, returning its persistent GC root slot (W4).
    /// The finalized value stays LIVE in the session heap (never deep-forced to
    /// data) and is applied later by reference. Runs during the suspending turn,
    /// so `gc_active_range`/`vmctx` are valid.
    fn tenure_finalized_payload(
        &mut self,
        machine: &mut CompiledEffectMachine,
        request_ptr: *mut u8,
    ) -> Result<crate::old_space::RootSlot, JitError> {
        if request_ptr.is_null() {
            return Err(JitError::Yield(crate::yield_type::YieldError::NullPointer));
        }
        // FinalizeWith(site, value): the value is field index 1. Read its pointer
        // out of the (WHNF Con) request. SAFETY: request_ptr is the rooted request
        // Con from the suspend arm; a `FinalizeWith` always has >= 2 fields.
        let value_ptr = unsafe {
            let nf = *(request_ptr.add(crate::layout::CON_NUM_FIELDS_OFFSET as usize) as *const u16)
                as usize;
            if nf < 2 {
                return Err(JitError::Yield(crate::yield_type::YieldError::Runtime(
                    crate::host_fns::RuntimeError::UserErrorMsg(format!(
                        "finalize request Con has {nf} fields, expected >= 2 (FinalizeWith site value)"
                    )),
                )));
            }
            *(request_ptr.add(crate::layout::CON_FIELDS_OFFSET as usize + 8) as *const *mut u8)
        };
        let from = self
            .machine_state
            .gc_active_range()
            .expect("GC state installed for the suspending finalize run");
        let from_range = (from.0 as *const u8, unsafe {
            from.0.add(from.1) as *const u8
        });
        let vmctx_ptr = machine.vmctx_mut() as *mut VMContext;
        // SAFETY: value_ptr is a live heap object in the nursery from-range; tenure
        // evacuates it into old-space and registers the returned slot as a
        // persistent root valid for the machine's life. `self.session` is
        // unaliased (reclaim not yet armed on the suspend path).
        let slot = unsafe {
            self.session
                .as_mut()
                .expect("session machine for a finalize tenure")
                .old_space
                .tenure(vmctx_ptr, value_ptr, from_range)
        };
        Ok(slot)
    }

    /// Take the [`RootSlot`] a value-plane bind tenured on its last suspendable
    /// completion (`run_fragment_suspendable_binding`/`resume_suspended_binding`),
    /// clearing it. `None` if the last run was not a bind or has already been
    /// taken. The caller reads this AFTER the machine moves back off the eval
    /// thread and records the `BindingEntry` against it.
    pub fn take_last_bound_root(&mut self) -> Option<crate::old_space::RootSlot> {
        self.last_bound_root.take()
    }

    /// Take the persistent root slot of a suspended `finalize @T closure`'s
    /// finalized VALUE (W4 finalize-by-reference), tenured at suspend time. The
    /// slot stays a registered persistent root for the machine's life (taking
    /// it here only removes the machine's own handle, not the registration), so
    /// a subsequent `run_child` that references it by slot address is GC-safe.
    /// `None` unless the machine suspended on a closure-valued finalize.
    pub fn take_finalized_root(&mut self) -> Option<crate::old_space::RootSlot> {
        self.suspended_finalized_root.take()
    }

    /// Run a pure (non-effectful) program to completion.
    ///
    /// Skips freer-simple effect dispatch entirely — calls the compiled function
    /// and converts the raw heap result directly to a Value. Use this for programs
    /// that don't use an `Eff` wrapper.
    pub fn run_pure(&mut self) -> Result<Value, JitError> {
        let func_id = self.func_id;
        self.run_pure_with_entry(func_id)
    }

    /// Shared pure-run body, parametrized by the entry `func_id`. [`Self::run_pure`]
    /// uses the machine's original entry; [`Self::run_fragment_pure`] passes an
    /// [`Self::add_function`]-minted fragment id. Same session lifecycle either way.
    fn run_pure_with_entry(&mut self, func_id: FuncId) -> Result<Value, JitError> {
        // L7: see run_with_entry's doc. Shared by `run_pure` and `run_fragment_pure`.
        assert!(
            self.suspended_continuation.is_none(),
            "run_pure/run_fragment_pure called while a continuation is suspended — \
             resume_suspended it first"
        );
        // Per-thread signal handler + altstack; see `run`. Idempotent.
        crate::signal_safety::install();

        // Install registries
        let mut _guard = self.install_registries();

        // SAFETY: get_function_ptr returns a finalized JIT code pointer. Transmuting to the
        // expected calling convention (vmctx -> result) is correct per our compilation contract.
        let func_ptr: unsafe extern "C" fn(*mut VMContext) -> *mut u8 =
            unsafe { std::mem::transmute(self.pipeline.get_function_ptr(func_id)) };
        let mut vmctx = if self.session.is_some() {
            self.make_session_vmctx()
        } else {
            self.nursery.make_vmctx(crate::host_fns::gc_trigger)
        };
        // SAFETY: machine_state outlives this run (owned by self).
        vmctx.machine_state = &mut self.machine_state as *mut MachineState;
        // Arm reclaim so Drop can recover active_buffer → session.heap.
        // SAFETY: &vmctx lives on this stack frame; VMContext has no custom Drop
        // so its bytes are valid when _guard drops (which is before run_pure returns).
        unsafe {
            _guard.arm_reclaim(&mut self.session as *mut _, &vmctx as *const _);
        }

        self.machine_state.reset_call_depth();
        crate::host_fns::set_exec_context("running pure computation");
        // SAFETY: Calling the JIT function through a valid function pointer with signal
        // protection for crash recovery. vmctx is freshly created from the nursery.
        let result_ptr: *mut u8 =
            unsafe { crate::signal_safety::with_signal_protection(|| func_ptr(&mut vmctx)) }
                .map_err(|e| JitError::Yield(runtime_error_or_signal(e.0)))?;

        // SAFETY: Resolving pending tail calls. vmctx.tail_callee/tail_arg are valid
        // heap pointers set by JIT tail-call sites. Code pointers in closures point to
        // finalized JIT functions. Signal protection guards each call.
        let result_ptr = unsafe { resolve_tail_calls_protected(&mut vmctx, result_ptr)? };

        // Check for runtime error FIRST — runtime_error now returns a poison
        // object instead of null, so we can't rely on null-check alone.
        if let Some(err) = crate::host_fns::take_runtime_error() {
            return Err(JitError::Yield(crate::yield_type::YieldError::from(err)));
        }
        if result_ptr.is_null() {
            return Err(JitError::Yield(crate::yield_type::YieldError::NullPointer));
        }

        // SAFETY: result_ptr is a valid heap pointer returned by the JIT.
        // vmctx_ptr is valid for forcing thunks during value conversion.
        let bridge_result = unsafe {
            let vmctx_ptr = &mut vmctx as *mut VMContext;
            crate::signal_safety::with_signal_protection(|| {
                heap_bridge::heap_to_value_forcing(result_ptr, vmctx_ptr)
            })
        }
        .map_err(JitError::Signal)?;

        // The bridge calls back into JIT via `heap_force`, which can trigger
        // `gc_trigger` — an external cancel observed there records
        // `RuntimeError::Cancelled` as the first cause, while the bridge
        // reports only its symptom (the forced thunk never completed).
        crate::host_fns::surface_error(bridge_result.map_err(JitError::HeapBridge))
    }

    // ----------------------------------------------------------------------
    // GHCi-style session re-entry (Wave 1 — components C, K).
    //
    // These freeze the codegen contracts the tidepool-repl session manager
    // (Wave 2) builds on. Implemented in Wave 1.B atop the 1.A lifecycle seam
    // (buffer retention, persistent roots, tenuring); see
    // plans/ghci-implementation-plan.md §4 (1.B).
    // ----------------------------------------------------------------------

    /// Compile an additional `CoreExpr` fragment into this machine's *live*
    /// `JITModule` and return its `FuncId`, without tearing down the existing
    /// code or heap. The new fragment may reference session bindings via
    /// `external_env` (Var-miss resolution to seeded heap pointers).
    ///
    /// Component C1: declare + define the fragment and re-run
    /// `finalize_definitions` (multi-round-safe in cranelift 0.129.1 — a new
    /// `FuncId` post-finalize carves a fresh arena segment, leaving round-1 code
    /// stable). `table` shapes the fragment exactly like the one-shot entry
    /// (`normalize` + datacon-env wrap + lit-wrapper tolerance), so re-entry is
    /// emission-identical to the original compile, only the destination differs.
    /// Returns the id for a later [`Self::run_fragment`] / [`Self::run_fragment_pure`].
    pub fn add_function(
        &mut self,
        name: &str,
        expr: &CoreExpr,
        table: &DataConTable,
        external_env: &crate::emit::ExternalEnv,
    ) -> Result<FuncId, JitError> {
        let shape_start = std::time::Instant::now();
        // Mirror compile_inner's tree shaping so the fragment is emitted exactly
        // like the original entry; only the JITModule destination differs (it is
        // already finalized — we add a fresh round).
        let expr = tidepool_repr::normalize(expr, table);
        // `normalize` is bracketed separately inside `shape`: it is a
        // whole-tree rebuild whose cost tracks fragment size, while the rest of
        // shaping tracks table size. One `shape_ms` bucket cannot tell those
        // two apart.
        let normalize_ms = shape_start.elapsed();
        // Pre-wrap reachable-constructor count: the fragment's own Core, before
        // wrap_with_datacon_env mechanically adds a reference to every table
        // constructor. This is the number the metadata-vs-reachable ratio needs;
        // core_cons measured downstream of the wrap (tidepool-codegen/src/emit/expr.rs's
        // fragment_stats line, a different tree) coincides with table_cons by
        // construction and cannot answer that question. Gated on the target
        // being enabled: an unconditional walk + HashSet allocation on every
        // compile would tax the hot path this instrument exists to measure.
        // Timed separately and subtracted out of `shape_ms` below so the
        // diagnostic walk doesn't inflate the metric it reports.
        let walk_start = std::time::Instant::now();
        let core_cons_prewrap_count = if log::log_enabled!(target: "tidepool::codegen", log::Level::Debug)
        {
            let mut set: rustc_hash::FxHashSet<tidepool_repr::DataConId> =
                rustc_hash::FxHashSet::default();
            for node in &expr.nodes {
                match node {
                    tidepool_repr::CoreFrame::Con { tag, .. } => {
                        set.insert(*tag);
                    }
                    tidepool_repr::CoreFrame::Case { alts, .. } => {
                        for alt in alts {
                            if let tidepool_repr::AltCon::DataAlt(id) = alt.con {
                                set.insert(id);
                            }
                        }
                    }
                    _ => {}
                }
            }
            set.len()
        } else {
            0
        };
        let walk_elapsed = walk_start.elapsed();
        let crate::datacon_env::WrappedExpr { expr, wraps } =
            crate::datacon_env::wrap_with_datacon_env(expr, table);
        // Boxed-literal wrapper tolerance is per-compile; refresh from this
        // fragment's table (see compile_inner). Runtime-inert — read only during
        // emission — so refreshing it does not perturb already-compiled code.
        self.pipeline.lit_wrappers = crate::emit::LitWrapperIds::from_table(table);
        // ACCUMULATE the primop constructor-id bundles (JsonDecode / ParseISO8601)
        // as fragments introduce constructors: upgrade None -> Some, never clobber
        // a resolved bundle. Each turn's table is a SUBSET of the session, so a
        // later turn that merely FORCES a primop-produced thunk — its own Core
        // may not reference Either/Value/I#/Text at all — still sees the ids a
        // turn that DID reference them resolved. (Without this, forcing a
        // JsonDecode/ParseISO8601 result in a sparse turn failed with
        // "constructors not in scope".) The machine reads these fields at every
        // run entry via `install_registries`.
        if let Some(ids) = tidepool_eval::json::JsonConIds::from_table(table) {
            self.json_con_ids = Some(ids);
        }
        if let Some(ids) = tidepool_eval::time::TimeConIds::from_table(table) {
            self.time_con_ids = Some(ids);
        }
        // Refresh `tags` too — re-resolve ConTags against THIS fragment's table
        // rather than leaving it frozen at whatever `compile_inner` saw at
        // bootstrap (plans/self-iterating-harness/12-contags-staleness-findings.md,
        // finding 1/1b). The asymmetry is deliberate, not an oversight:
        //   Err -> Ok: install. Mirrors json_con_ids/time_con_ids' accumulate-
        //     never-clobber intent — a later turn's table may supply a freer
        //     constructor (Val/E/Union/Leaf/Node) that bootstrap's table lacked,
        //     and without this a session stays permanently `MissingConTags`
        //     even once the table can classify (finding 1b, deterministic).
        //   Ok -> Ok (re-resolved): install. An accumulated session table is a
        //     superset of the bootstrap one, so this is a no-op in practice,
        //     but re-resolving against the turn's own table rather than
        //     assuming stability is the honest rule.
        //   Ok -> Err: do NOT clobber. Overwriting an established `Ok` with a
        //     fresh `Err` would break a session whose later turn happens to
        //     carry a sparser table than a prior turn did.
        if let Ok(refreshed) = ConTags::from_table(table) {
            self.tags = Ok(refreshed);
        }
        let nodes = expr.nodes.len();
        // Subtract the pre-wrap diagnostic walk's own time: it sits inside this
        // window (it needs the pre-wrap tree, which wrap_with_datacon_env then
        // consumes) but is not part of the shaping work this bucket measures.
        let shape_ms = shape_start.elapsed() - walk_elapsed;

        let functions_defined_before = self.pipeline.functions_defined();
        let blocks_emitted_before = self.pipeline.blocks_emitted();
        let dce_before = self.pipeline.dce_scan;

        let emit_start = std::time::Instant::now();
        let func_id =
            crate::emit::expr::compile_expr(&mut self.pipeline, &expr, name, external_env)
                .map_err(JitError::Compilation)?;
        let emit_ms = emit_start.elapsed();

        let finalize_start = std::time::Instant::now();
        // Multi-round finalize: finalize_definitions is safe to re-run; finalize()
        // drains only THIS round's pending stack maps and appends them to the
        // registry (round-1 maps were drained on the first finalize).
        self.pipeline.finalize()?;
        let finalize_ms = finalize_start.elapsed();

        let funcs = self.pipeline.functions_defined() - functions_defined_before;
        let blocks = self.pipeline.blocks_emitted() - blocks_emitted_before;
        let dce_delta = self.pipeline.dce_scan.delta_since(&dce_before);
        log::debug!(
            target: "tidepool::codegen",
            "add_function name={name} table_cons={table_cons} core_cons_prewrap={core_cons_prewrap} \
             wrapped_cons={wrapped_cons} nodes={nodes} normalize_ms={normalize_ms:.3} shape_ms={shape_ms:.3} \
             emit_ms={emit_ms:.3} finalize_ms={finalize_ms:.3} \
             funcs={funcs} blocks={blocks} dce_calls={dce_calls} dce_nodes={dce_nodes} dce_ms={dce_ms:.3}",
            table_cons = table.iter().count(),
            core_cons_prewrap = core_cons_prewrap_count,
            wrapped_cons = wraps.len(),
            normalize_ms = normalize_ms.as_secs_f64() * 1000.0,
            shape_ms = shape_ms.as_secs_f64() * 1000.0,
            emit_ms = emit_ms.as_secs_f64() * 1000.0,
            finalize_ms = finalize_ms.as_secs_f64() * 1000.0,
            dce_calls = dce_delta.calls,
            dce_nodes = dce_delta.nodes_walked,
            dce_ms = dce_delta.elapsed.as_secs_f64() * 1000.0,
        );

        Ok(func_id)
    }

    /// Run a previously-[`add_function`](Self::add_function)ed fragment against
    /// this machine's live, machine-owned heap, dispatching effects through the
    /// handler HList exactly as [`Self::run`] does for the one-shot entry.
    ///
    /// Component C2: like `run`, but targets `func_id` instead of the machine's
    /// original entry, reusing the persistent heap via the 1.A buffer-retention
    /// contract (`install_registries` re-points GC state at the retained buffer).
    pub fn run_fragment<U, H: DispatchEffect<U>>(
        &mut self,
        func_id: FuncId,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
    ) -> Result<Value, JitError> {
        self.run_with_entry(func_id, table, handlers, user)
    }

    /// Pure sibling of [`Self::run_fragment`]: run an `add_function`-minted
    /// fragment whose result is a plain value (no `Eff` wrapper) against the
    /// retained session heap. Mirrors [`Self::run_pure`]. Used by the converge
    /// proof, where a reference fragment (`case x of C n -> n`) resolves a
    /// tenured session value purely.
    pub fn run_fragment_pure(&mut self, func_id: FuncId) -> Result<Value, JitError> {
        self.run_pure_with_entry(func_id)
    }

    /// The value-plane **bind primitive**: run a pure entry, deep-force its
    /// result to normal form (component K), tenure the NF value into the session
    /// old-space (component E), register its persistent GC root (component D),
    /// and return the stable [`RootSlot`](crate::old_space::RootSlot) a later
    /// fragment resolves through its `ExternalEnv`.
    ///
    /// This assembles 1.A's tenure/persistent-root machinery with 1.B's
    /// `deep_force`. The tenure happens while GC state is still installed and the
    /// result pointer is live (before the per-run `RegistryGuard` reclaims the
    /// nursery buffer), so the tenured copy and its slot outlive the run.
    ///
    /// # Panics
    /// Panics if called on a non-session machine (no old-space to tenure into).
    pub fn run_pure_and_bind(
        &mut self,
        func_id: FuncId,
    ) -> Result<crate::old_space::RootSlot, JitError> {
        assert!(
            self.session.is_some(),
            "run_pure_and_bind requires a session machine (compile_session)"
        );
        // L7: see run_with_entry's doc.
        assert!(
            self.suspended_continuation.is_none(),
            "run_pure_and_bind called while a continuation is suspended — \
             resume_suspended it first"
        );
        // Per-thread signal handler + altstack; see `run`. Idempotent.
        crate::signal_safety::install();

        let mut _guard = self.install_registries();

        // SAFETY: finalized JIT code pointer; calling convention per contract.
        let func_ptr: unsafe extern "C" fn(*mut VMContext) -> *mut u8 =
            unsafe { std::mem::transmute(self.pipeline.get_function_ptr(func_id)) };
        let mut vmctx = self.make_session_vmctx();
        // SAFETY: machine_state outlives this run (owned by self).
        vmctx.machine_state = &mut self.machine_state as *mut MachineState;

        self.machine_state.reset_call_depth();
        crate::host_fns::set_exec_context("running pure computation (bind)");

        // All fallible steps live in this closure so `arm_reclaim` below runs
        // unconditionally on every exit, success OR error (mirrors
        // `run_fragment_and_bind`'s `drive_to_done(..).and_then(..)` shape).
        // Skipping arm_reclaim on an error path used to leave `session.cursor`
        // stale: `RegistryGuard::drop`'s `clear_run_scratch` frees the active
        // buffer (possibly GC-grown up to 1 GiB) regardless of whether reclaim
        // ran, but only reclaim writes back the buffer + correct high-water
        // cursor. The next run would then compute `alloc_ptr` from the stale
        // cursor against a fresh, smaller nursery — an out-of-bounds pointer.
        let result = (|| -> Result<crate::old_space::RootSlot, JitError> {
            // SAFETY: calling the JIT function through a valid pointer, signal-protected.
            let result_ptr: *mut u8 =
                unsafe { crate::signal_safety::with_signal_protection(|| func_ptr(&mut vmctx)) }
                    .map_err(|e| JitError::Yield(runtime_error_or_signal(e.0)))?;
            // SAFETY: resolves pending tail calls (vmctx tail slots are valid).
            let result_ptr = unsafe { resolve_tail_calls_protected(&mut vmctx, result_ptr)? };

            if let Some(err) = crate::host_fns::take_runtime_error() {
                return Err(JitError::Yield(crate::yield_type::YieldError::from(err)));
            }
            if result_ptr.is_null() {
                return Err(JitError::Yield(crate::yield_type::YieldError::NullPointer));
            }

            // K — deep-force the result to NF before tenuring (no thunks survive into
            // old-space; the no-write-barrier tenuring invariant assumes NF data).
            // SAFETY: result_ptr is a valid heap object; vmctx is the active context.
            let nf_ptr = unsafe {
                crate::signal_safety::with_signal_protection(|| {
                    crate::host_fns::deep_force(&mut vmctx as *mut VMContext, result_ptr)
                })
            }
            .map_err(JitError::Signal)?;
            if let Some(err) = crate::host_fns::take_runtime_error() {
                return Err(JitError::Yield(crate::yield_type::YieldError::from(err)));
            }

            // E/D — tenure the NF closure out of the nursery into old-space and
            // register its persistent root. gc_active_range is the nursery from-range
            // (still installed; the guard has not dropped). The tenured copy lives in
            // old-space arenas, independent of the buffer the guard reclaims.
            let from = self
                .machine_state
                .gc_active_range()
                .expect("GC state installed for the bind run");
            let from_range = (from.0 as *const u8, unsafe {
                from.0.add(from.1) as *const u8
            });
            let vmctx_ptr = &mut vmctx as *mut VMContext;
            // SAFETY: nf_ptr is a live heap object inside the nursery from-range;
            // tenure evacuates its closure and registers the returned slot (via
            // vmctx_ptr's machine_state) as a persistent root valid for the
            // machine's life.
            let slot = unsafe {
                self.session
                    .as_mut()
                    .expect("session machine")
                    .old_space
                    .tenure(vmctx_ptr, nf_ptr, from_range)
            };
            Ok(slot)
        })();

        // Arm reclaim LAST (after all `self.session` access) so the guard's raw
        // pointer to `self.session` is not aliased by an intervening `&mut`
        // borrow — and unconditionally on both Ok and Err (Finding 4). On drop
        // the guard recovers the live buffer + high-water cursor → session.
        // heap/cursor for the next run. SAFETY: &vmctx lives on this frame;
        // VMContext has no custom Drop so its bytes are valid at drop.
        unsafe {
            _guard.arm_reclaim(&mut self.session as *mut _, &vmctx as *const _);
        }
        result
    }

    /// The effectful value-plane **bind primitive**: run `func_id` through the
    /// freer-simple effect step loop (dispatching through `handlers`), and at
    /// `Yield::Done(ptr)` apply the BIND sequence from `run_pure_and_bind`:
    /// optionally `deep_force` to NF (`forced = true` → Tier0 data; `false` →
    /// Tier1 closure, tenure as-is), tenure into old-space, register the
    /// persistent root, and return the stable
    /// [`RootSlot`](crate::old_space::RootSlot) a later fragment resolves via
    /// `ExternalEnv`.
    ///
    /// **Why this must exist (not reusing `run_pure_and_bind`):** a bind turn
    /// compiles `result = do { x <- action; pure x } :: Eff stack T`. The Core
    /// is a freer-simple `Eff` tree, NOT a bare `T`. `run_pure_and_bind` calls
    /// the entry once and roots the immediate return — for an `Eff` result that
    /// is the `Val`-leaf wrapper, not the underlying value. The value only
    /// appears at `Yield::Done(ptr)` AFTER the effect step loop reduces the
    /// tree. This method runs the loop and then applies the bind sequence.
    ///
    /// **Reclaim ordering (UAF risk):** follows `run_pure_and_bind` (NOT
    /// `run_with_entry`). Do NOT arm reclaim before the step loop — the Done
    /// arm accesses `self.session` for tenure, and `arm_reclaim` stores a raw
    /// `*mut self.session`; the two cannot alias. Arm reclaim LAST after the
    /// loop exits, after all `self.session` access.
    ///
    /// # Panics
    /// Panics if called on a non-session machine (no old-space to tenure into).
    pub fn run_fragment_and_bind<U, H: DispatchEffect<U>>(
        &mut self,
        func_id: FuncId,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
        forced: bool,
    ) -> Result<crate::old_space::RootSlot, JitError> {
        assert!(
            self.session.is_some(),
            "run_fragment_and_bind requires a session machine (compile_session)"
        );
        // L7: see run_with_entry's doc.
        assert!(
            self.suspended_continuation.is_none(),
            "run_fragment_and_bind called while a continuation is suspended — \
             resume_suspended it first"
        );

        let tags = self.tags.map_err(JitError::MissingConTags)?;

        // Per-thread signal handler + altstack; idempotent (see run_with_entry).
        crate::signal_safety::install();

        // Install registries
        let mut _guard = self.install_registries();

        // SAFETY: get_function_ptr returns a finalized JIT code pointer. Transmuting to the
        // expected calling convention (vmctx -> result) is correct per our compilation contract.
        let func_ptr: unsafe extern "C" fn(*mut VMContext) -> *mut u8 =
            unsafe { std::mem::transmute(self.pipeline.get_function_ptr(func_id)) };
        let vmctx = self.make_session_vmctx();

        let mut machine = CompiledEffectMachine::new(func_ptr, vmctx, tags);
        // SAFETY: machine_state outlives this run (owned by self).
        machine.vmctx_mut().machine_state = &mut self.machine_state as *mut MachineState;
        // NOTE: do NOT arm reclaim before the step loop — the Done arm accesses
        // self.session (for tenure) and arm_reclaim stores a raw *mut self.session;
        // the two cannot alias. Follow run_pure_and_bind's ordering: tenure first
        // inside the loop, arm_reclaim LAST after the loop exits.

        let result = drive_to_done(
            &mut machine,
            &self.cancel_flag,
            table,
            handlers,
            user,
            "stepping effectful computation (bind)",
            "",
        )
        .and_then(|ptr| {
            if let Some(err) = crate::host_fns::take_runtime_error() {
                return Err(JitError::Yield(crate::yield_type::YieldError::from(err)));
            }
            if ptr.is_null() {
                return Err(JitError::Yield(crate::yield_type::YieldError::NullPointer));
            }

            // K — optionally deep-force to NF before tenuring (Tier0).
            // Tier1 closures are NOT forced (they are callable code, not data).
            // SAFETY: ptr is a valid heap object; machine.vmctx_mut() is the
            // active VMContext for forcing thunks.
            let nf_ptr = if forced {
                let nf = unsafe {
                    crate::signal_safety::with_signal_protection(|| {
                        crate::host_fns::deep_force(machine.vmctx_mut() as *mut VMContext, ptr)
                    })
                }
                .map_err(JitError::Signal)?;
                // Forcing may have triggered a gc_trigger cancel observation;
                // prefer that over a symptomatic bridge error.
                if let Some(err) = crate::host_fns::take_runtime_error() {
                    return Err(JitError::Yield(crate::yield_type::YieldError::from(err)));
                }
                nf
            } else {
                ptr
            };

            // E/D — tenure the (optionally forced) closure out of the nursery
            // into old-space and register its persistent root. gc_active_range
            // is the nursery from-range (still installed; the guard has not
            // dropped). The tenured copy lives in old-space arenas, independent
            // of the buffer the guard reclaims.
            let from = self
                .machine_state
                .gc_active_range()
                .expect("GC state installed for the bind run");
            let from_range = (from.0 as *const u8, unsafe {
                from.0.add(from.1) as *const u8
            });
            let vmctx_ptr = machine.vmctx_mut() as *mut VMContext;
            // SAFETY: nf_ptr is a live heap object inside the nursery
            // from-range; tenure evacuates its closure and registers the
            // returned slot (via vmctx_ptr's machine_state) as a persistent
            // root valid for the machine's life.
            let slot = unsafe {
                self.session
                    .as_mut()
                    .expect("session machine")
                    .old_space
                    .tenure(vmctx_ptr, nf_ptr, from_range)
            };
            Ok(slot)
        });

        // Arm reclaim LAST (after all `self.session` access — tenure is in the
        // epilogue above) so the guard's raw pointer to `self.session` is not
        // aliased by an intervening `&mut` borrow. On drop the guard recovers
        // the live buffer + high-water cursor → session.heap/cursor for the
        // next run. SAFETY: machine.vmctx_mut() points into `machine` on this
        // stack frame; CompiledEffectMachine has no custom Drop so its bytes
        // are valid when _guard drops (machine drops first but the stack frame
        // is still live).
        unsafe {
            _guard.arm_reclaim(&mut self.session as *mut _, machine.vmctx_mut() as *const _);
        }
        result
    }

    /// Multi-binder effectful bind: run `func_id` through the effect step loop,
    /// and at `Yield::Done(tuple_ptr)` project each field of the result tuple,
    /// optionally deep-force Tier-0 fields, tenure each field into old-space, and
    /// return one [`RootSlot`](crate::old_space::RootSlot) per component.
    ///
    /// `forced_mask[i] = true` → deep-force field `i` before tenuring (Tier-0
    /// data); `false` → tenure as-is (Tier-1 closure). The caller (session.rs
    /// `run_multi_bind`) zips the returned slots with the binder metadata.
    ///
    /// **Field order invariant**: `forced_mask` must align with the tuple fields in
    /// source order — the same order as the `pure (a, b, …)` wrapper and the
    /// binders in the JSON sidecar. The assertion on `n_actual` guards against
    /// shape mismatches.
    ///
    /// **Reclaim ordering** follows `run_fragment_and_bind`: tenure ALL fields
    /// inside the Done arm (before arm_reclaim), then arm reclaim LAST after the
    /// loop exits so the `*mut self.session` raw pointer is not aliased by an
    /// intervening `&mut` borrow.
    ///
    /// # Panics
    /// Panics if called on a non-session machine.
    pub fn run_fragment_and_bind_projected<U, H: DispatchEffect<U>>(
        &mut self,
        func_id: FuncId,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
        n_fields: usize,
    ) -> Result<Vec<crate::old_space::RootSlot>, JitError> {
        assert!(
            self.session.is_some(),
            "run_fragment_and_bind_projected requires a session machine"
        );
        assert!(
            n_fields > 0,
            "run_fragment_and_bind_projected requires at least one field"
        );
        // L7: see run_with_entry's doc.
        assert!(
            self.suspended_continuation.is_none(),
            "run_fragment_and_bind_projected called while a continuation is \
             suspended — resume_suspended it first"
        );

        let tags = self.tags.map_err(JitError::MissingConTags)?;

        crate::signal_safety::install();
        let mut _guard = self.install_registries();

        let func_ptr: unsafe extern "C" fn(*mut VMContext) -> *mut u8 =
            unsafe { std::mem::transmute(self.pipeline.get_function_ptr(func_id)) };
        let vmctx = self.make_session_vmctx();
        let mut machine = CompiledEffectMachine::new(func_ptr, vmctx, tags);
        // SAFETY: machine_state outlives this run (owned by self).
        machine.vmctx_mut().machine_state = &mut self.machine_state as *mut MachineState;
        // NOTE: do NOT arm reclaim before the step loop (same ordering as
        // run_fragment_and_bind — tenure is in the Done arm).

        let result = drive_to_done(
            &mut machine,
            &self.cancel_flag,
            table,
            handlers,
            user,
            "stepping effectful computation (multi-bind)",
            " (multi-bind)",
        )
        .and_then(|tuple_ptr| {
            if let Some(err) = crate::host_fns::take_runtime_error() {
                return Err(JitError::Yield(crate::yield_type::YieldError::from(err)));
            }
            if tuple_ptr.is_null() {
                return Err(JitError::Yield(crate::yield_type::YieldError::NullPointer));
            }

            // GC-safe projection protocol:
            // 1. deep_force the WHOLE TUPLE first. deep_force internally
            //    registers every pending parent as a Rust GC root and re-reads
            //    field slots from the live (possibly relocated) parent after
            //    each heap_force — so no pointer is cached across a GC.
            //    Returns nf_tuple: the post-GC NF address with all field slots
            //    updated to live NF children. Closures (TAG_CLOSURE) are
            //    forced to WHNF and left as-is.
            let nf_tuple = unsafe {
                crate::signal_safety::with_signal_protection(|| {
                    crate::host_fns::deep_force(machine.vmctx_mut() as *mut VMContext, tuple_ptr)
                })
            }
            .map_err(JitError::Signal)?;
            if let Some(err) = crate::host_fns::take_runtime_error() {
                return Err(JitError::Yield(crate::yield_type::YieldError::from(err)));
            }

            // 2. Validate arity from the NF (post-GC) object.
            let n_actual = unsafe {
                *(nf_tuple.add(crate::layout::CON_NUM_FIELDS_OFFSET as usize) as *const u16)
                    as usize
            };
            if n_actual != n_fields {
                return Err(JitError::Yield(crate::yield_type::YieldError::Runtime(
                    crate::host_fns::RuntimeError::UserErrorMsg(format!(
                        "multi-bind: result tuple has {} fields, expected {}",
                        n_actual, n_fields
                    )),
                )));
            }

            // 3. Capture from_range AFTER deep_force (GC may have changed the
            //    active region). tenure() is pure Rust — no JIT GC fires — so
            //    this range stays valid for all field tenures.
            let from = self
                .machine_state
                .gc_active_range()
                .expect("GC state installed for the bind run");
            let from_range = (from.0 as *const u8, unsafe {
                from.0.add(from.1) as *const u8
            });
            let vmctx_ptr = machine.vmctx_mut() as *mut VMContext;

            // 4. Project each field from nf_tuple and tenure. nf_tuple stays
            //    valid across all tenure() calls (no JIT GC). deep_force
            //    already wrote live NF pointers into each slot.
            let mut slots = Vec::with_capacity(n_fields);
            for i in 0..n_fields {
                let field_ptr = unsafe {
                    *(nf_tuple.add(crate::layout::CON_FIELDS_OFFSET as usize + 8 * i)
                        as *const *mut u8)
                };
                let slot = unsafe {
                    self.session
                        .as_mut()
                        .expect("session machine")
                        .old_space
                        .tenure(vmctx_ptr, field_ptr, from_range)
                };
                slots.push(slot);
            }
            Ok(slots)
        });

        // Arm reclaim LAST (after all self.session access — tenure is in the
        // epilogue above). Same UAF ordering as run_fragment_and_bind.
        unsafe {
            _guard.arm_reclaim(&mut self.session as *mut _, machine.vmctx_mut() as *const _);
        }
        result
    }

    /// The single-compile `it`-binding primitive: run `func_id` through the
    /// effect step loop, and at `Yield::Done(tuple_ptr)` — the result of the
    /// wrapped `pure (it, toWire it)` — bridge field 1 (the render) into an
    /// OWNED [`Value`] first, then tenure field 0 (`it` itself) alone,
    /// returning both. Replaces the old two-compile `run_bare_expr` (one
    /// compile to bind `it` via [`Self::run_fragment_and_bind`], a second,
    /// separate compile to render `toWire it`).
    ///
    /// **Why field1-before-field0-tenure is load-bearing:** when `toWire` is
    /// the identity (`toWire :: Aeson.Value -> Aeson.Value`, e.g. a bare
    /// `pure input`), field 0 and field 1 resolve to the exact SAME heap
    /// object. [`heap_bridge::heap_to_value_forcing`] returns a COMPLETE DEEP
    /// COPY — every leaf is owned Rust data, no pointer into the JIT heap
    /// survives the call — so bridging field 1 into `rendered` FIRST makes it
    /// immune to whatever `tenure` does to that shared object afterward.
    /// Tenuring field 0 SECOND (and ONLY field 0 — field 1 is never tenured)
    /// means at most one object in this call ever gets forwarded, so the
    /// aliasing corruption a naive `pure (it, toWire it)` +
    /// `run_fragment_and_bind_projected` hit (independently tenuring both
    /// fields of a shared object — see that method's doc and
    /// `old_space::tenure`'s forward-skip fix) cannot recur here.
    ///
    /// `field0_forced`: mirrors `run_fragment_and_bind`'s `forced` flag —
    /// `true` (Tier0Data) deep-forces field 0 to NF before tenuring; `false`
    /// (Tier1 closure) tenures field 0 as-is, unforced.
    ///
    /// **Reclaim ordering** follows `run_fragment_and_bind`/`_projected`:
    /// tenure inside the Done arm (before arm_reclaim), arm reclaim LAST after
    /// the loop exits.
    ///
    /// # Panics
    /// Panics if called on a non-session machine.
    pub fn run_fragment_and_bind_render<U, H: DispatchEffect<U>>(
        &mut self,
        func_id: FuncId,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
        field0_forced: bool,
    ) -> Result<(crate::old_space::RootSlot, Value), JitError> {
        assert!(
            self.session.is_some(),
            "run_fragment_and_bind_render requires a session machine"
        );
        // L7: see run_with_entry's doc.
        assert!(
            self.suspended_continuation.is_none(),
            "run_fragment_and_bind_render called while a continuation is \
             suspended — resume_suspended it first"
        );

        let tags = self.tags.map_err(JitError::MissingConTags)?;

        crate::signal_safety::install();
        let mut _guard = self.install_registries();

        let func_ptr: unsafe extern "C" fn(*mut VMContext) -> *mut u8 =
            unsafe { std::mem::transmute(self.pipeline.get_function_ptr(func_id)) };
        let vmctx = self.make_session_vmctx();
        let mut machine = CompiledEffectMachine::new(func_ptr, vmctx, tags);
        // SAFETY: machine_state outlives this run (owned by self).
        machine.vmctx_mut().machine_state = &mut self.machine_state as *mut MachineState;
        // NOTE: do NOT arm reclaim before the step loop (same ordering as
        // run_fragment_and_bind / _projected — tenure is in the Done arm).

        let result = drive_to_done(
            &mut machine,
            &self.cancel_flag,
            table,
            handlers,
            user,
            "stepping effectful computation (bind-render)",
            " (bind-render)",
        )
        .and_then(|tuple_ptr| {
            if let Some(err) = crate::host_fns::take_runtime_error() {
                return Err(JitError::Yield(crate::yield_type::YieldError::from(err)));
            }
            if tuple_ptr.is_null() {
                return Err(JitError::Yield(crate::yield_type::YieldError::NullPointer));
            }

            // `Yield::Done` (effect_machine::parse_result) already forces the
            // Val field to WHNF before returning it, so tuple_ptr is
            // guaranteed a real Con here (never a thunk) — safe to read its
            // header directly, no additional WHNF force needed.
            let tag = unsafe { *tuple_ptr };
            if tag != crate::layout::TAG_CON {
                return Err(JitError::Yield(
                    crate::yield_type::YieldError::UnexpectedTag(tag),
                ));
            }
            let n_actual = unsafe {
                *(tuple_ptr.add(crate::layout::CON_NUM_FIELDS_OFFSET as usize) as *const u16)
                    as usize
            };
            if n_actual != 2 {
                return Err(JitError::Yield(crate::yield_type::YieldError::Runtime(
                    crate::host_fns::RuntimeError::UserErrorMsg(format!(
                        "bind-render: result tuple has {} fields, expected 2",
                        n_actual
                    )),
                )));
            }

            // Read-only, no GC-capable calls in between — both field
            // pointers are consistent with the (already-WHNF) tuple_ptr.
            let field0_ptr = unsafe {
                *(tuple_ptr.add(crate::layout::CON_FIELDS_OFFSET as usize) as *const *mut u8)
            };
            let field1_ptr = unsafe {
                *(tuple_ptr.add(crate::layout::CON_FIELDS_OFFSET as usize + 8) as *const *mut u8)
            };

            let vmctx_ptr = machine.vmctx_mut() as *mut VMContext;

            // Root field0_ptr across the field1 bridge below: bridging can
            // force thunks reachable from field1's subtree, which can
            // allocate and trigger a minor GC that relocates field0's object
            // (whether or not it aliases field1). Registering it here keeps
            // it live and GC-updated so the value we tenure afterward is
            // correct post-GC.
            let mut field0_ptr = field0_ptr;
            // SAFETY: vmctx_ptr is the active run's VMContext; the scope
            // covers exactly the field1 bridge call below.
            let _root0 = unsafe { heap_bridge::RootScope::new(vmctx_ptr) };
            // SAFETY: the slot lives on this frame until _root0 drops.
            unsafe {
                crate::host_fns::register_rust_root(vmctx_ptr, &mut field0_ptr as *mut *mut u8);
            }

            // READ-BEFORE-TENURE (load-bearing): bridge field1 (the render)
            // into a fully OWNED Value before field0 is forced or tenured.
            // heap_to_value_forcing's result retains no pointer into the JIT
            // heap, so it is unaffected by whatever tenure() below does to
            // field0's object — even when field0 and field1 alias.
            let bridge_res = unsafe {
                crate::signal_safety::with_signal_protection(|| {
                    heap_bridge::heap_to_value_forcing(field1_ptr, vmctx_ptr)
                })
            }
            .map_err(JitError::Signal)?;
            if let Some(err) = crate::host_fns::take_runtime_error() {
                return Err(JitError::Yield(crate::yield_type::YieldError::from(err)));
            }
            let rendered =
                crate::host_fns::surface_error(bridge_res.map_err(JitError::HeapBridge))?;

            // field0_ptr is no longer needed as a GC root past this point —
            // `deep_force` (if field0_forced) roots its own traversal, and
            // `tenure` triggers no JIT GC.
            drop(_root0);

            // Force (iff field0_forced, mirroring run_fragment_and_bind's
            // tier-driven forcing) and tenure field0 ONLY. field1 is never
            // tenured — it was already fully consumed into `rendered` above.
            let nf_field0 = if field0_forced {
                let nf = unsafe {
                    crate::signal_safety::with_signal_protection(|| {
                        crate::host_fns::deep_force(
                            machine.vmctx_mut() as *mut VMContext,
                            field0_ptr,
                        )
                    })
                }
                .map_err(JitError::Signal)?;
                if let Some(err) = crate::host_fns::take_runtime_error() {
                    return Err(JitError::Yield(crate::yield_type::YieldError::from(err)));
                }
                nf
            } else {
                field0_ptr
            };

            // Capture from_range AFTER any forcing above (GC may have
            // changed the active region) — same ordering as `_projected`.
            let from = self
                .machine_state
                .gc_active_range()
                .expect("GC state installed for the bind run");
            let from_range = (from.0 as *const u8, unsafe {
                from.0.add(from.1) as *const u8
            });
            let vmctx_ptr = machine.vmctx_mut() as *mut VMContext;
            let slot = unsafe {
                self.session
                    .as_mut()
                    .expect("session machine")
                    .old_space
                    .tenure(vmctx_ptr, nf_field0, from_range)
            };
            Ok((slot, rendered))
        });

        // Arm reclaim LAST (after all self.session access — tenure is in the
        // epilogue above). Same UAF ordering as run_fragment_and_bind.
        unsafe {
            _guard.arm_reclaim(&mut self.session as *mut _, machine.vmctx_mut() as *const _);
        }
        result
    }

    /// Register a session-scoped GC root slot that survives across runs (i.e.
    /// across `RegistryGuard` drops), unlike the per-run rust roots.
    ///
    /// Wave 1.A (component D): fill this machine's persistent-roots registry
    /// so a tenured binding's root is appended to `perform_gc`'s root set and
    /// is NOT cleared by the per-run `clear_run_scratch`. Takes a slot
    /// pointer (`*mut *mut u8`) like `host_fns::register_rust_root`.
    ///
    /// # Safety
    /// The caller guarantees that `slot` is non-null, points to a valid
    /// `*mut u8` heap-pointer location, and remains valid and dereferenceable
    /// until the session ends (the `JitEffectMachine` is dropped) — the copying
    /// GC will read and rewrite `*slot` in place on every collection until then.
    /// A slot freed or moved before machine teardown is a use-after-free.
    pub unsafe fn register_persistent_root(&self, slot: *mut *mut u8) {
        // Delegates directly to this machine's own MachineState (not through
        // the vmctx-gated free fn) — `self.machine_state` IS the handle that
        // fn would otherwise have to look up. `free_session_heap` (machine
        // drop) clears the registry. SAFETY: forwarded to the caller's
        // contract documented above.
        self.machine_state.register_persistent_root(slot);
    }

    /// Number of persistent GC roots currently registered on this machine
    /// (test/diagnostic accessor). Reads `self.machine_state` directly —
    /// unlike the vmctx-gated `host_fns::persistent_roots_count` free fn,
    /// this works whether or not a run is currently in flight, since a
    /// `JitEffectMachine` always owns its `MachineState`.
    pub fn persistent_roots_count(&self) -> usize {
        self.machine_state.persistent_roots_count()
    }

    /// Number of write-barrier remembered slots currently registered on this
    /// machine (test/diagnostic accessor) — the barrier's sibling of
    /// `persistent_roots_count`. Reads `self.machine_state` directly, same
    /// rationale as `persistent_roots_count`.
    pub fn remembered_slots_count(&self) -> usize {
        self.machine_state.remembered_slots_count()
    }

    /// Whether this machine is currently suspended at a typed yield (`Ask`),
    /// holding a stowed continuation awaiting `resume_suspended`.
    pub fn is_suspended(&self) -> bool {
        self.suspended_continuation.is_some()
    }

    /// Number of stowed GC roots currently registered (test/diagnostic
    /// accessor — 1 while a nested child is running against a suspended parent,
    /// 0 otherwise). Segment 40.
    pub fn stowed_roots_count(&self) -> usize {
        self.machine_state.stowed_roots_count()
    }

    /// Read-only heap/GC snapshot (observatory heap pane) — EXISTING counters
    /// only, no new instrumentation inside the collector. `nursery_bytes` is
    /// the nursery's total capacity; `live_bytes` is the session heap's bump
    /// high-water mark (`SessionState::cursor` — bytes allocated since the
    /// last GC, or since bootstrap if none has run yet); `gc_count` is
    /// [`MachineState::gc_generation`], bumped once per actual collection.
    pub fn heap_stats(&self) -> HeapStats {
        let live_bytes = self.session.as_ref().map(|s| s.cursor).unwrap_or(0);
        HeapStats {
            nursery_bytes: self.nursery.size(),
            live_bytes,
            gc_count: self.machine_state.gc_generation(),
        }
    }

    // ----------------------------------------------------------------------
    // Segment 40 — nested child runs on a suspended machine.
    //
    // While a parent turn is suspended at a typed yield (`runLLMTurn`/`Ask`,
    // `suspended_continuation` is `Some`), CHILD fragment runs can execute
    // against the SAME machine — reading the parent's bindings zero-copy —
    // provided the parent's stowed continuation is a REGISTERED GC ROOT so a
    // child's collection evacuates it rather than freeing it.
    //
    // The temporal "no GC runs on a suspended machine" argument (the L7 asserts)
    // is REPLACED, for the nested case only, by this registered root. The L7
    // asserts on the plain entries (`run`/`run_pure`/`run_fragment`/`*_and_bind`)
    // stay UNCHANGED: a plain entry started while a continuation is stowed and
    // UNREGISTERED is still an illegal state and still panics. The nested-child
    // entries below are the ONLY sanctioned way to run while suspended, and they
    // register the root first.
    // ----------------------------------------------------------------------

    /// Enter nested-child mode: MOVE the stowed continuation out of
    /// `suspended_continuation` into a heap-stable `Box` cell, register that
    /// cell in `stowed_roots`, and increment `nested_child_depth`. Returns a
    /// [`NestedChildGuard`] whose `Drop` reads the (GC-current) pointer back out
    /// and restores it into `suspended_continuation`, deregisters the root, and
    /// decrements the depth.
    ///
    /// Moving the pointer OUT of `suspended_continuation` for the child's
    /// duration is load-bearing two ways: (1) `suspended_continuation` reads
    /// `None` while the child runs, so the plain run entries' L7 asserts pass
    /// naturally — the child fragment goes through `run_fragment*` exactly like
    /// any turn — and (2) the continuation is protected NOT by the (now-absent)
    /// temporal argument but by the `stowed_roots` registration on the
    /// heap-stable cell, which every child collection traces and rewrites in
    /// place. On guard drop the machine's `suspended_continuation` again points
    /// at the (possibly relocated) continuation.
    ///
    /// # Panics
    /// Panics if the machine is not suspended (no continuation to root) — a
    /// nested child requires a suspended parent by construction.
    fn enter_nested_child(&mut self) -> NestedChildGuard {
        let cont = self
            .suspended_continuation
            .take()
            .expect("enter_nested_child on a machine that is not suspended");
        // Heap-stable cell: the machine moves between threads (stow XOR run),
        // but the Box's pointee address is a stable heap allocation, so the
        // registered slot address stays valid across the move.
        let mut cell = Box::new(cont);
        let slot: *mut *mut u8 = &mut *cell;
        self.stowed_root_cell = Some(cell);
        // SAFETY: `slot` is the address of the Box's inner cell, stable for the
        // Box's life (until the guard drops and puts the pointer back). The GC
        // reads and rewrites `*slot` in place on every collection until then.
        self.machine_state.register_stowed_root(slot);
        self.nested_child_depth += 1;
        NestedChildGuard {
            machine_state: &self.machine_state as *const MachineState,
            suspended_continuation: &mut self.suspended_continuation as *mut Option<*mut u8>,
            stowed_root_cell: &mut self.stowed_root_cell as *mut Option<Box<*mut u8>>,
            nested_child_depth: &mut self.nested_child_depth as *mut usize,
            slot,
        }
    }

    /// Run a fragment as a CHILD against this suspended parent, dispatching
    /// effects through the handler HList exactly as [`Self::run_fragment`] does.
    /// The parent's stowed continuation is GC-rooted for the child's duration
    /// (see [`Self::enter_nested_child`]); a child collection — including heap
    /// doubling — evacuates it, so the parent resumes correctly afterward.
    ///
    /// The child fragment reads the parent's session bindings zero-copy through
    /// its `external_env` (resolved when the fragment was `add_function`-minted),
    /// against the SAME retained session heap. Module accretion is inert for the
    /// parent: adding a child fragment does not perturb the parent's stowed
    /// continuation.
    ///
    /// # Panics
    /// Panics if the machine is not currently suspended.
    pub fn run_child_fragment<U, H: DispatchEffect<U>>(
        &mut self,
        func_id: FuncId,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
    ) -> Result<Value, JitError> {
        assert!(
            self.suspended_continuation.is_some(),
            "run_child_fragment requires a suspended parent (call run_fragment on an idle machine)"
        );
        let _nested = self.enter_nested_child();
        // With the continuation moved into the registered stowed cell,
        // `suspended_continuation` is None — the plain run entry's L7 assert
        // passes, and the fragment drives byte-identically to any turn.
        self.run_with_entry(func_id, table, handlers, user)
    }

    /// Pure sibling of [`Self::run_child_fragment`] — run an `add_function`-minted
    /// pure fragment as a child against the suspended parent's retained heap.
    ///
    /// # Panics
    /// Panics if the machine is not currently suspended.
    pub fn run_child_fragment_pure(&mut self, func_id: FuncId) -> Result<Value, JitError> {
        assert!(
            self.suspended_continuation.is_some(),
            "run_child_fragment_pure requires a suspended parent"
        );
        let _nested = self.enter_nested_child();
        self.run_pure_with_entry(func_id)
    }
}

/// RAII proof that a nested child is running against a suspended parent
/// (segment 40). On drop it reads the (GC-current) continuation pointer back
/// out of the heap-stable stowed cell and restores it into the machine's
/// `suspended_continuation`, deregisters the stowed root, drops the cell, and
/// decrements the nested-child depth — leaving the machine exactly as suspended
/// as it was on entry, but with the continuation pointer updated to wherever the
/// child's collections relocated it.
///
/// All four raw pointers point into the owning `JitEffectMachine`. The guard is
/// a local in `run_child_fragment*` and drops at that method's end, strictly
/// within the method's `&mut self` scope — so `self` cannot have moved or
/// dropped while the guard is alive. This mirrors `RegistryGuard`, which
/// likewise holds raw pointers into its owning call frame rather than a borrow
/// (so `self` stays free for the run call it wraps).
struct NestedChildGuard {
    machine_state: *const MachineState,
    suspended_continuation: *mut Option<*mut u8>,
    stowed_root_cell: *mut Option<Box<*mut u8>>,
    nested_child_depth: *mut usize,
    slot: *mut *mut u8,
}

impl Drop for NestedChildGuard {
    fn drop(&mut self) {
        // SAFETY: all pointers target the owning JitEffectMachine's fields,
        // live for the guard's whole scope (the guard is a local in the child
        // run method, which holds `&mut self`). The stowed cell holds the
        // GC-current continuation pointer (rewritten in place by any child
        // collection through the registered slot); read it back out and restore
        // it so the parent stays suspended on the relocated continuation.
        unsafe {
            (*self.machine_state).deregister_stowed_root(self.slot);
            let cell = (*self.stowed_root_cell)
                .take()
                .expect("stowed cell present for the guard's life");
            *self.suspended_continuation = Some(*cell);
            debug_assert!(
                *self.nested_child_depth > 0,
                "nested_child_depth underflow — double drop"
            );
            *self.nested_child_depth = (*self.nested_child_depth).saturating_sub(1);
        }
    }
}

impl Drop for JitEffectMachine {
    fn drop(&mut self) {
        // Clear this machine's persistent-root registry (whose slots point
        // into the session heap Vec, which drops with self after this).
        // Harmless for one-shot machines (free_session_heap does nothing if
        // GC state is already absent, and no persistent roots are
        // registered). Operates directly on self.machine_state — always
        // clears exactly this machine's own registries, never a different
        // one (see the `free_session_heap` doc on `MachineState`).
        if self.session.is_some() {
            // Retire every old-space arena BEFORE the arena Vec<u8>s
            // themselves drop (which happens when `self.session` drops,
            // after this fn body returns): forgets any remembered write-
            // barrier slot pointing into that arena so it never outlives the
            // memory it points into, and deregisters the range so a
            // diagnostic pass never reads freed memory as live old-space.
            for (start, end) in self.machine_state.old_space_arena_ranges() {
                self.machine_state.retire_old_space_arena(start, end);
            }
            self.machine_state.free_session_heap();
        }
    }
}

/// Resolve pending tail calls with signal protection.
///
/// # Safety
/// vmctx must have valid tail_callee/tail_arg if non-null.
unsafe fn resolve_tail_calls_protected(
    vmctx: &mut VMContext,
    result: *mut u8,
) -> Result<*mut u8, JitError> {
    let mut ptr = result;
    while ptr.is_null() && !vmctx.tail_callee.is_null() {
        // External cancellation safepoint — see the rationale in
        // `host_fns::trampoline_resolve`. Without this check, an infinite
        // tail-recursive loop never yields control back to the caller even
        // when cancellation has been requested.
        if crate::host_fns::check_cancel_and_set_error(vmctx) {
            vmctx.tail_callee = std::ptr::null_mut();
            vmctx.tail_arg = std::ptr::null_mut();
            ptr = crate::host_fns::error_poison_ptr();
            break;
        }

        let callee = vmctx.tail_callee;
        let arg = vmctx.tail_arg;
        vmctx.tail_callee = std::ptr::null_mut();
        vmctx.tail_arg = std::ptr::null_mut();
        machine_state(vmctx).reset_call_depth();
        let code_ptr =
            *(callee.add(crate::layout::CLOSURE_CODE_PTR_OFFSET as usize) as *const usize);
        let func: unsafe extern "C" fn(*mut VMContext, *mut u8, *mut u8) -> *mut u8 =
            std::mem::transmute(code_ptr);
        ptr = crate::signal_safety::with_signal_protection(|| func(vmctx, callee, arg))
            .map_err(|e| JitError::Yield(runtime_error_or_signal(e.0)))?;
    }
    Ok(ptr)
}

/// Normalized effect-response materialization: a stream to park, a Value to
/// convert eagerly, or an already-materialized heap pointer (kill-switch
/// drains). Shared by the one effect-drive loop below.
enum ResponsePlan {
    Park(crate::host_fns::ParkedStream),
    Eager(tidepool_eval::value::Value),
    Ready(*mut u8),
}

/// Outcome of the shared effect step loop ([`drive_effect_loop`]): the turn
/// completed with a Done heap pointer, or it SUSPENDED at the caller's
/// `suspend_tag` (threadless suspension — E2). `Suspended` carries the bridged
/// request `Value` (the caller extracts prompt/meta) and the raw continuation
/// heap pointer (the caller stows it; the machine's session heap is retained
/// across the suspension). The non-suspend callers pass `suspend_tag = None`
/// and never observe `Suspended`.
enum DriveOutcome {
    Done(*mut u8),
    Suspended {
        request: tidepool_eval::value::Value,
        /// The raw heap pointer to the request `Con` (rooted for the arm). W4:
        /// a `finalize`'s value field crosses by reference, so `finish_suspendable`
        /// reaches back into this Con to tenure the finalized value when the
        /// bridged `request` carries a [`heap_bridge::CLOSURE_SENTINEL`] placeholder.
        request_ptr: *mut u8,
        continuation: *mut u8,
    },
}

/// Whether a bridged suspend request carries a [`heap_bridge::CLOSURE_SENTINEL`]
/// placeholder among its top-level Con fields — the tolerant bridge's marker
/// that a field was a live closure it declined to materialize (W4). Only the
/// direct fields of the request Con are checked: a `finalize`'s value is field
/// 1 of `FinalizeWith`, and no other suspend request (`Ask`/`RunLLMTurn`) can
/// legally contain a closure, so a nested sentinel would itself be a bug.
fn request_carries_closure_sentinel(request: &tidepool_eval::value::Value) -> bool {
    match request {
        tidepool_eval::value::Value::Con(_, fields) => fields.iter().any(|f| {
            matches!(
                f,
                tidepool_eval::value::Value::Con(id, _) if *id == heap_bridge::CLOSURE_SENTINEL
            )
        }),
        _ => false,
    }
}

/// Result of a suspendable turn ([`JitEffectMachine::run_suspendable`] /
/// [`JitEffectMachine::resume_suspended`]): the turn produced a value, or it
/// suspended at the ask boundary carrying the bridged request `Value` (the
/// continuation is stowed inside the machine, ready for `resume_suspended`).
pub enum SuspendableOutcome {
    /// The turn ran to completion; `Value` is the bridged result.
    Completed(tidepool_eval::value::Value),
    /// The turn suspended at the ask boundary. `request` is the bridged `Ask`
    /// request; the machine holds the continuation internally.
    Suspended {
        request: tidepool_eval::value::Value,
        /// W4 finalize-by-reference: `true` when the suspend request was a
        /// `finalize @T closure` — the finalized VALUE (field 1 of the request
        /// Con) has been tenured into old-space and its persistent root slot
        /// stashed on the machine ([`JitEffectMachine::take_finalized_root`]).
        /// The bridged `request` carries a [`heap_bridge::CLOSURE_SENTINEL`] in
        /// that field's place. `false` for an ordinary `Ask`/`RunLLMTurn`
        /// suspension (or a `finalize` of a plain DATA value, which bridges
        /// fully and needs no by-reference handoff).
        has_finalized_closure: bool,
    },
}

/// How a suspended turn is re-entered ([`JitEffectMachine::resume_suspended`]).
pub enum ResumeInput {
    /// Feed the (already-validated, bridged) answer value into the suspended
    /// ask and continue driving.
    Answer(tidepool_eval::value::Value),
    /// Abort the suspended ask WITHOUT running the continuation (a stowed
    /// machine has no thread) — returns `JitError::Effect(EffectError::
    /// Handler("ask aborted by caller: {reason}"))` directly, the same
    /// terminal outcome a pre-E2 caller-abort produced. This does NOT touch
    /// the first-cause cell / record `RuntimeError::Cancelled` — that cause
    /// is reserved for the gate/timeout abort path, not a caller-supplied
    /// abort reason.
    Abort(String),
}

/// Drive the freer-simple effect step loop to `Yield::Done`: step the machine,
/// bridge + dispatch each effect request, materialize the response (lazy park
/// or eager), and resume — returning the final Done heap pointer for the
/// caller's epilogue (value bridging in `run_with_entry`; deep-force/tenure in
/// the `*_and_bind` variants; tuple projection in `_projected`).
///
/// This is THE effect-boundary body, shared by all three run methods — they
/// differ only in their Done epilogues, exec-context labels, and
/// reclaim-arming position. It owns the continuation GC-rooting, the request
/// bridge + runtime-error precedence, the effect-dispatch cancellation
/// safepoint, and the Stream/Complete response planning with the lazy-spine
/// re-park.
fn drive_to_done<U, H: DispatchEffect<U>>(
    machine: &mut CompiledEffectMachine,
    cancel_flag: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    table: &DataConTable,
    handlers: &mut H,
    user: &U,
    exec_start: &str,
    resume_suffix: &str,
) -> Result<*mut u8, JitError> {
    let yield_result = initial_step(machine, exec_start);
    // suspend_tag = None: the ask-suspend branch is never taken, so this drives
    // exactly as the pre-E2 inline loop did — byte-identical non-suspend path.
    // Threadless suspension is opt-in via `JitEffectMachine::run_suspendable`,
    // which passes `Some(ask_tag)`.
    match drive_effect_loop(
        machine,
        cancel_flag,
        table,
        handlers,
        user,
        resume_suffix,
        None,
        yield_result,
    )? {
        DriveOutcome::Done(ptr) => Ok(ptr),
        DriveOutcome::Suspended { .. } => {
            unreachable!("drive_to_done passes suspend_tag=None; the effect loop never suspends")
        }
    }
}

/// The initial `machine.step()` for a fresh drive: reset call depth, set the
/// exec-context label, step under signal protection. Shared by
/// [`drive_to_done`] and [`JitEffectMachine::run_suspendable`].
fn initial_step(machine: &mut CompiledEffectMachine, exec_start: &str) -> Yield {
    // SAFETY: machine.vmctx_mut()'s machine_state was set by the caller before
    // entering the effect loop.
    unsafe { machine_state(machine.vmctx_mut() as *mut VMContext) }.reset_call_depth();
    crate::host_fns::set_exec_context(exec_start);
    // SAFETY: with_signal_protection wraps the JIT call with sigsetjmp for
    // crash recovery; machine.step() calls the JIT function through a valid
    // function pointer.
    match unsafe { crate::signal_safety::with_signal_protection(|| machine.step()) } {
        Ok(y) => y,
        Err(e) => signal_error_to_yield(e),
    }
}

/// The shared freer-simple effect step loop, factored out of [`drive_to_done`]
/// so the same body serves the non-suspending run AND threadless suspension.
///
/// `suspend_tag = Some(t)`: a `Yield::Request` with `tag >= t` unwinds as
/// [`DriveOutcome::Suspended`] (after bridging the request and while the
/// continuation is still valid), instead of dispatching to a handler. `t` is
/// the first INTERPOSED (unhandled) tag — every tag from there on is
/// unhandled by construction, so this threshold test is what lets
/// `Ask`/`RunLLMTurn`/`Finalize` (self-iterating-harness WS-B) share this one
/// suspend arm without each needing its own comparison.
/// `suspend_tag = None`: every effect dispatches exactly as the pre-E2 inline
/// loop did — the non-suspend path is byte-identical.
///
/// `yield_result` is the entry Yield: a fresh [`initial_step`] for a new turn,
/// or a `machine.resume(..)` of the stowed continuation for a re-entry.
#[allow(clippy::too_many_arguments)]
fn drive_effect_loop<U, H: DispatchEffect<U>>(
    machine: &mut CompiledEffectMachine,
    cancel_flag: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    table: &DataConTable,
    handlers: &mut H,
    user: &U,
    resume_suffix: &str,
    suspend_tag: Option<u64>,
    mut yield_result: Yield,
) -> Result<DriveOutcome, JitError> {
    loop {
        match yield_result {
            Yield::Done(ptr) => return Ok(DriveOutcome::Done(ptr)),
            Yield::Request {
                tag,
                request,
                continuation,
            } => {
                // Root the continuation for the whole arm: request-forcing
                // (heap_force runs thunk code that can allocate → GC) and
                // response materialization (host_alloc_gc in
                // alloc_stream_tail_thunk / build_cons_cells) can collect
                // while the JIT stack is unwound — an UNROOTED continuation
                // tree is not evacuated and from-space is freed, so
                // `machine.resume(continuation, …)` would read freed memory.
                // The GC rewrites the rooted slot in place; resume reads the
                // updated pointer.
                let mut continuation = continuation;
                let vmctx_ptr = machine.vmctx_mut() as *mut VMContext;
                // SAFETY: vmctx_ptr is the active run's VMContext.
                let _cont_root = unsafe { heap_bridge::RootScope::new(vmctx_ptr) };
                // SAFETY: the slot lives on this frame until the arm ends
                // (after resume); _cont_root truncates the registry on drop.
                unsafe {
                    crate::host_fns::register_rust_root(
                        vmctx_ptr,
                        &mut continuation as *mut *mut u8,
                    );
                }
                // Root the raw request pointer too, alongside the continuation:
                // a suspended `finalize @T closure` (self-iterating-harness W4)
                // passes the finalized value by REFERENCE, so the harness reaches
                // BACK into this request Con's value field live after the suspend.
                // The bridge below may itself GC (thunk forcing); registering the
                // request slot keeps field(1)'s subtree evacuated + GC-updated.
                let mut request = request;
                // SAFETY: the slot lives on this frame until the arm ends; the
                // request subtree is also reachable from the rooted continuation,
                // so it survives the suspension regardless.
                unsafe {
                    crate::host_fns::register_rust_root(vmctx_ptr, &mut request as *mut *mut u8);
                }
                // SAFETY: request is a valid heap pointer from the JIT effect dispatch.
                // A suspend request uses the TOLERANT bridge: a `finalize`'s value
                // field may be a closure (`TAG_CLOSURE`), which has no data `Value`
                // representation — the tolerant bridge substitutes a placeholder
                // (`CLOSURE_SENTINEL`) so the leading `site`/`prompt` fields still
                // bridge for the classifier, while the real closure crosses by
                // reference (`request_ptr` below). `Ask`/`RunLLMTurn` requests carry
                // no closure, so the policy is behavior-preserving for them.
                let bridge_res = unsafe {
                    let vmctx_ptr = machine.vmctx_mut() as *mut VMContext;
                    crate::signal_safety::with_signal_protection(|| {
                        heap_bridge::heap_to_value_forcing_tolerant(request, vmctx_ptr)
                    })
                }
                .map_err(JitError::Signal)?;
                // Request forcing can record a first cause (e.g. a cancel in
                // `gc_trigger`); the bridge outcome is only its symptom.
                let req_val =
                    crate::host_fns::surface_error(bridge_res.map_err(JitError::HeapBridge))?;
                log::debug!(target: "tidepool::effects", "effect tag={} request={:?}", tag, req_val);
                // E2 threadless suspension: `suspend_tag` is the FIRST interposed
                // (unhandled) tag — the position right after the last effect with
                // a real handler. Every tag at or beyond it is unhandled by
                // construction (Ask, and — self-iterating-harness WS-B —
                // RunLLMTurn/Finalize, always appended consecutively after the
                // handled stack), so the threshold test `tag >= suspend_tag`
                // catches ALL of them through this one arm: no per-effect
                // duplication, `Ask`/`RunLLMTurn`/`Finalize` share this exact
                // suspend path. A stack with only one interposed effect (the
                // pre-WS-B norm, and today's ordinary eval/repl stacks) has
                // `suspend_tag` as the only tag >= it, so this is behavior-
                // preserving there. Unwind carrying the bridged request + the
                // continuation instead of dispatching. `continuation` here is the
                // post-request-bridge value (the arm's `register_rust_root`
                // updated it in place through any GC during forcing). The arm's
                // `_cont_root` drops as we return, releasing the run-scoped root;
                // the raw pointer stays valid because the session heap buffer is
                // retained across the suspension (no GC runs while stowed), and
                // `resume_suspended` re-roots it before its answer materialization
                // can collect.
                if suspend_tag.is_some_and(|t| tag >= t) {
                    return Ok(DriveOutcome::Suspended {
                        request: req_val,
                        request_ptr: request,
                        continuation,
                    });
                }
                let cx = EffectContext::with_user(table, user);
                // A dispatcher that aborts at its `PauseGate` checkpoint
                // records `RuntimeError::Cancelled` as the first cause before
                // returning `EffectError::Handler`, so a gate-fired timeout
                // surfaces the same cause as a flag-fired one. Ordinary
                // handler errors record no cause and pass through unchanged.
                let response = crate::host_fns::surface_error(
                    handlers
                        .dispatch(tag, &req_val, &cx)
                        .map_err(JitError::from),
                )?;

                // External cancellation safepoint at the effect-dispatch
                // boundary. The handler we just called may itself have flipped
                // the cancel flag (a watchdog handler is the canonical case);
                // the JIT-internal safepoints (gc_trigger, trampoline_resolve)
                // only fire on tail-recursive or heavy-allocating Haskell, so
                // freer-simple effect loops would otherwise observe the cancel
                // only as an eventual unrelated error. Checking here gives
                // prompt unwind for the realistic handler-driven scenario
                // without depending on the shape of the compiled program.
                if cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err(JitError::Yield(crate::yield_type::YieldError::Runtime(
                        crate::host_fns::RuntimeError::Cancelled,
                    )));
                }

                // Materialize the handler response and resume the continuation.
                // Extracted so `resume_suspended` re-enters a stowed turn
                // through the identical path (see the helper's doc).
                yield_result = materialize_response_and_resume(
                    machine,
                    continuation,
                    response,
                    table,
                    tag,
                    resume_suffix,
                )?;
            }
            Yield::Error(e) => return Err(JitError::Yield(e)),
        }
    }
}

/// Materialize a handler [`tidepool_effect::Response`] into a heap pointer and
/// resume the machine's `continuation` with it, returning the next [`Yield`].
///
/// Factored verbatim out of the effect loop so [`JitEffectMachine::resume_suspended`]
/// re-enters a stowed turn through the EXACT same materialization path (lazy
/// `Stream` park, long-spine re-park, eager `value_to_heap`) — one body, no
/// drift-prone second copy. The non-suspend loop calls this once per effect
/// exactly as before, so its behavior is unchanged.
///
/// `continuation` is GC-rooted here for the duration: response materialization
/// (`value_to_heap` / `alloc_stream_tail_thunk` / `materialize_cons_list`) can
/// allocate and collect, which would move the continuation out from under the
/// `machine.resume` below. The in-loop caller also holds its own arm root
/// across request forcing; this extra registration harmlessly overlaps it
/// (both slots track the same pointer through a GC).
fn materialize_response_and_resume(
    machine: &mut CompiledEffectMachine,
    mut continuation: *mut u8,
    response: tidepool_effect::Response,
    table: &DataConTable,
    tag: u64,
    resume_suffix: &str,
) -> Result<Yield, JitError> {
    let vmctx_ptr = machine.vmctx_mut() as *mut VMContext;
    // SAFETY: vmctx_ptr is the active run's VMContext; the slot lives on this
    // frame until _root truncates the registry on drop.
    let _root = unsafe { heap_bridge::RootScope::new(vmctx_ptr) };
    // SAFETY: &mut continuation is a stable stack slot for the duration below.
    unsafe {
        crate::host_fns::register_rust_root(vmctx_ptr, &mut continuation as *mut *mut u8);
    }

    // Response materialization. Two channels:
    //
    // Stream: the handler parked nothing and built nothing —
    // elements convert per-pull, chunk-by-chunk, as Haskell forces
    // tails (`take k` of a huge listing converts ~one chunk; an
    // infinite producer is a legitimate infinite list). With the
    // TIDEPOOL_LAZY_RESULTS=0 kill-switch the stream drains
    // eagerly through the node cap instead.
    //
    // Complete: classic Value. Long list spines are flattened BY
    // VALUE (iterative dismantle) and re-parked as a pre-converted
    // stream — a deep spine must never reach a recursive Drop or
    // recursive value_to_heap (~3 stack frames per cell overflow
    // the eval thread; the fault lands outside signal protection
    // and silently kills the thread — see .tidepool/crash.log).
    // The node cap remains as a backstop for large non-list
    // responses.
    const LAZY_SPINE_THRESHOLD_NODES: usize = 2_000;
    const MAX_EFFECT_RESPONSE_NODES: usize = 100_000;
    let lazy_enabled = std::env::var("TIDEPOOL_LAZY_RESULTS")
        .map(|v| v != "0")
        .unwrap_or(true);

    let plan = match response {
        tidepool_effect::Response::Stream(s) => {
            let (mut source, cons_id, nil_id) = s.into_parts();
            if lazy_enabled {
                ResponsePlan::Park(crate::host_fns::ParkedStream {
                    source,
                    cons_tag: cons_id.0,
                    nil_tag: nil_id.0,
                    table: table.clone(),
                })
            } else {
                // Kill-switch: drain through the node cap. (This
                // makes infinite producers a clean TooLarge error
                // instead of divergence.)
                let mut items = Vec::new();
                let mut nodes = 0usize;
                let mut too_large = false;
                while let Some(r) = source.next_value(table) {
                    let v = r.map_err(|e| JitError::from(EffectError::Bridge(e)))?;
                    nodes += 3 + v.node_count();
                    items.push(v);
                    if nodes > MAX_EFFECT_RESPONSE_NODES {
                        too_large = true;
                        break;
                    }
                }
                if too_large {
                    return Err(JitError::EffectResponseTooLarge {
                        nodes,
                        limit: MAX_EFFECT_RESPONSE_NODES,
                    });
                }
                let p = unsafe {
                    crate::signal_safety::with_signal_protection(|| {
                        crate::host_fns::materialize_cons_list(
                            machine.vmctx_mut(),
                            cons_id.0,
                            nil_id.0,
                            &items,
                        )
                    })
                }
                .map_err(JitError::Signal)?;
                if let Some(err) = crate::host_fns::take_runtime_error() {
                    return Err(JitError::Yield(crate::yield_type::YieldError::from(err)));
                }
                ResponsePlan::Ready(p)
            }
        }
        tidepool_effect::Response::Complete(resp_val) => {
            let spine =
                probe_list_spine(&resp_val).filter(|&(_, _, len)| len > LAZY_SPINE_THRESHOLD_NODES);
            match spine {
                Some((cons_tag, nil_tag, len)) if lazy_enabled => {
                    // Re-park the dismantled spine as a
                    // pre-converted stream: one registry, one chunk
                    // materializer for both channels.
                    let items = dismantle_list_spine(resp_val, len);
                    ResponsePlan::Park(crate::host_fns::ParkedStream {
                        source: Box::new(crate::host_fns::ReadySource::new(items)),
                        cons_tag,
                        nil_tag,
                        // Pre-converted: table never consulted.
                        table: tidepool_repr::DataConTable::new(),
                    })
                }
                Some((cons_tag, nil_tag, len)) => {
                    // Kill-switch: eager iterative materialization,
                    // cap still applies.
                    let items = dismantle_list_spine(resp_val, len);
                    let nodes = 3 * len + items.iter().map(|v| v.node_count()).sum::<usize>();
                    if nodes > MAX_EFFECT_RESPONSE_NODES {
                        return Err(JitError::EffectResponseTooLarge {
                            nodes,
                            limit: MAX_EFFECT_RESPONSE_NODES,
                        });
                    }
                    let p = unsafe {
                        crate::signal_safety::with_signal_protection(|| {
                            crate::host_fns::materialize_cons_list(
                                machine.vmctx_mut(),
                                cons_tag,
                                nil_tag,
                                &items,
                            )
                        })
                    }
                    .map_err(JitError::Signal)?;
                    if let Some(err) = crate::host_fns::take_runtime_error() {
                        return Err(JitError::Yield(crate::yield_type::YieldError::from(err)));
                    }
                    ResponsePlan::Ready(p)
                }
                None => ResponsePlan::Eager(resp_val),
            }
        }
    };
    let resp_ptr = match plan {
        ResponsePlan::Ready(p) => p,
        ResponsePlan::Park(stream) => {
            let id = crate::host_fns::park_stream(stream);
            // SAFETY: vmctx is valid with installed GC state. One
            // GC-and-retry via the shared `gc_retry` helper, matching the
            // Eager arm below: `continuation` is already a registered
            // rust_root (above), so the retry's collection evacuates it
            // safely, and a transient nursery-full for this (small,
            // fixed-size) tail-thunk allocation is recoverable rather than
            // fatal.
            //
            // NESTED RETRY, not a second policy layer: `alloc_stream_tail_thunk`
            // already retries internally via `host_alloc_gc` (also `gc_retry`-
            // based), so on the failure path this can run alloc→gc→alloc→gc
            // (inner) →alloc→gc→alloc (outer) — up to two collections, not
            // `gc_retry`'s documented one. This outer wrap exists as
            // defence-in-depth against that inner retry being removed or
            // this allocation growing past what one collection can satisfy,
            // not because one collection is insufficient today (it isn't —
            // see the load test's module doc for the evidence).
            let p = unsafe {
                crate::signal_safety::with_signal_protection(|| {
                    heap_bridge::gc_retry(
                        vmctx_ptr,
                        |p: &*mut u8| p.is_null(),
                        || crate::host_fns::alloc_stream_tail_thunk(machine.vmctx_mut(), id, 0),
                    )
                })
            }
            .map_err(JitError::Signal)?;
            if p.is_null() {
                return Err(JitError::HeapBridge(
                    heap_bridge::BridgeError::NurseryExhausted,
                ));
            }
            p
        }
        ResponsePlan::Eager(resp_val) => {
            let nodes = resp_val.node_count();
            if nodes > MAX_EFFECT_RESPONSE_NODES {
                return Err(JitError::EffectResponseTooLarge {
                    nodes,
                    limit: MAX_EFFECT_RESPONSE_NODES,
                });
            }
            // SAFETY: Converting a Value back to a heap object in the
            // nursery, with one GC-and-retry via the shared `gc_retry`
            // helper (matching every other value_to_heap call site:
            // primops.rs eitherDecode/parseISO8601, streaming.rs
            // build_cons_cells/stream_element): `continuation` is already a
            // registered rust_root (above), so the retry's collection
            // evacuates it safely, and a transient nursery-full at
            // response-materialization time is recoverable rather than
            // fatal.
            let conv = unsafe {
                crate::signal_safety::with_signal_protection(|| {
                    heap_bridge::gc_retry(
                        vmctx_ptr,
                        |r: &Result<*mut u8, heap_bridge::BridgeError>| {
                            matches!(r, Err(heap_bridge::BridgeError::NurseryExhausted))
                        },
                        || heap_bridge::value_to_heap(&resp_val, machine.vmctx_mut()),
                    )
                })
            }
            .map_err(JitError::Signal)?;
            match conv {
                Ok(p) => p,
                Err(e) => return Err(JitError::HeapBridge(e)),
            }
        }
    };
    // SAFETY: as above.
    unsafe { machine_state(machine.vmctx_mut() as *mut VMContext) }.reset_call_depth();
    crate::host_fns::set_exec_context(&format!(
        "resuming after effect tag={}{}",
        tag, resume_suffix
    ));
    // SAFETY: continuation and resp_ptr are valid nursery heap pointers.
    // resume applies the continuation tree to the response.
    Ok(
        match unsafe {
            crate::signal_safety::with_signal_protection(|| machine.resume(continuation, resp_ptr))
        } {
            Ok(y) => y,
            Err(e) => signal_error_to_yield(e),
        },
    )
}

/// Signal-boundary adapter for `host_fns::surface_error`: the raw signal is
/// the symptomatic fallback (a first cause like `BadFunPtrTag` is recorded by
/// `debug_app_check` before the JIT crashes and outranks it). Names the
/// faulting JIT function in the diagnostics when the fault address is known.
fn runtime_error_or_signal(sig: i32) -> crate::yield_type::YieldError {
    let fault_addr = crate::signal_safety::FAULTING_ADDR.with(|c| c.get());
    if fault_addr != 0 {
        if let Some(name) = crate::debug::lookup_lambda_by_address(fault_addr) {
            crate::host_fns::push_diagnostic(format!(
                "Signal {} in JIT function: {} (addr=0x{:x})",
                sig, name, fault_addr
            ));
        }
    }
    crate::host_fns::surface_error::<std::convert::Infallible, _>(Err(
        crate::yield_type::YieldError::Signal(sig),
    ))
    .unwrap_err()
}

/// Detect a cons-list spine by reference: a chain of 2-field Cons sharing one
/// DataConId, terminated by a 0-field Con. Returns (cons_tag, nil_tag, len).
/// Tags are read from the spine itself — no DataConTable lookup needed.
/// Iterative, walks the full spine to validate the terminator.
fn probe_list_spine(val: &tidepool_eval::value::Value) -> Option<(u64, u64, usize)> {
    use tidepool_eval::value::Value;
    let mut len = 0usize;
    let mut cons_tag: Option<u64> = None;
    let mut cur = val;
    loop {
        match cur {
            Value::Con(id, fields) if fields.len() == 2 => {
                match cons_tag {
                    None => cons_tag = Some(id.0),
                    Some(t) if t == id.0 => {}
                    Some(_) => return None, // mixed 2-field constructors: not a list
                }
                len += 1;
                cur = &fields[1];
            }
            Value::Con(id, fields) if fields.is_empty() => {
                return cons_tag.map(|c| (c, id.0, len));
            }
            _ => return None,
        }
    }
}

/// Dismantle a probe-validated cons spine BY VALUE: each element is moved out
/// and each cell freed iteratively, one at a time. This is the load-bearing
/// detail — letting a deep spine hit `Value`'s recursive destructor costs ~3
/// stack frames per cons cell, which overflows the eval thread's stack on
/// responses past a few thousand elements (SIGSEGV outside signal protection
/// → silent thread exit → caller hang).
fn dismantle_list_spine(
    val: tidepool_eval::value::Value,
    len: usize,
) -> Vec<tidepool_eval::value::Value> {
    use tidepool_eval::value::Value;
    let mut items = Vec::with_capacity(len);
    let mut cur = val;
    loop {
        // `ref mut` + pop: Value implements Drop, so fields can't move out
        // by pattern. (Value's Drop is itself iterative, so even handing a
        // deep spine to the destructor is safe now — this dismantle just
        // avoids building the worklist twice.)
        match cur {
            Value::Con(_, ref mut fields) if fields.len() == 2 => {
                let tail = fields.pop().expect("len checked");
                let head = fields.pop().expect("len checked");
                items.push(head);
                // The emptied cell (and its Vec) drops shallowly here.
                cur = tail;
            }
            // Probe validated the terminator: nothing deep remains.
            _ => break,
        }
    }
    items
}

fn signal_error_to_yield(e: crate::signal_safety::SignalError) -> Yield {
    Yield::Error(runtime_error_or_signal(e.0))
}

/// A5 (segment 40) — the deepseq-style NF check on a data-kinded resume answer.
///
/// A bridged answer `Value` is produced by `heap_to_value_forcing`, which forces
/// each node to WHNF as it walks — so a genuine bottom (`undefined`/`⊥`, a lazy
/// poison closure) is already raised at that bridge boundary as a `JitError`,
/// never reaching this point as a `Value`. This walk is the defense-in-depth
/// backstop the spec mandates: it rejects any answer carrying a residual
/// **unforced thunk** (`ThunkRef`) — the shape a not-fully-forced bottom would
/// take — as a retryable error, so the caller's continuation is NOT consumed.
///
/// Iterative (explicit work stack — data can be arbitrarily deep) with an
/// address-keyed visited set on `Con` payloads so shared/cyclic data terminates.
/// `Con`/`Lit`/`ByteArray` are normal-form data; `ThunkRef` is a bottom-reject;
/// `Closure`/`JoinCont`/`ConFun` cannot occur in a data-kinded answer
/// (function-bearing types were rejected at extract, segment 10) but are treated
/// as a reject too, since they are not first-order NF data.
fn answer_force_nf(root: &tidepool_eval::value::Value) -> Result<(), String> {
    use tidepool_eval::value::Value;
    let mut work: Vec<&Value> = vec![root];
    let mut visited: std::collections::HashSet<*const Vec<Value>> =
        std::collections::HashSet::new();
    while let Some(v) = work.pop() {
        match v {
            Value::Lit(_) | Value::ByteArray(_) => {}
            Value::Con(_, fields) => {
                // Dedup shared/cyclic sub-graphs by the payload Vec's address.
                if visited.insert(fields as *const Vec<Value>) {
                    for f in fields {
                        work.push(f);
                    }
                }
            }
            Value::ThunkRef(id) => {
                return Err(format!("unforced thunk {id} in answer"));
            }
            Value::Closure { .. } => {
                return Err("function-bearing value (closure) in answer".to_string());
            }
            Value::JoinCont { .. } => {
                return Err("join-point value in answer".to_string());
            }
            Value::ConFun(id, arity, args) => {
                return Err(format!(
                    "partially-applied constructor (Con#{} {}/{}) in answer",
                    id.0,
                    args.len(),
                    arity
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::yield_type::YieldError;
    use serial_test::serial;

    /// Regression test: when a RuntimeError is pending and a signal fires,
    /// prefer the RuntimeError (more specific) over the raw signal number.
    /// This prevents "JIT signal: unknown signal" when the real cause is
    /// something like BadFunPtrTag(255).
    #[test]
    fn test_runtime_error_preferred_over_signal() {
        crate::machine_state::test_support::with_test_machine(|| {
            // Set a pending runtime error via public API (kind=0 = DivisionByZero)
            crate::host_fns::runtime_error(0);

            // Signal fires after the runtime error was set
            let err = runtime_error_or_signal(libc::SIGBUS);

            // Should get DivisionByZero, not Signal(SIGBUS)
            assert_eq!(
                err,
                YieldError::Runtime(crate::host_fns::RuntimeError::DivisionByZero)
            );
        });
    }

    /// When no RuntimeError is pending, the signal number comes through.
    #[test]
    fn test_signal_passthrough_without_runtime_error() {
        // Ensure no pending error
        crate::host_fns::take_runtime_error();

        let err = runtime_error_or_signal(libc::SIGILL);
        assert_eq!(err, YieldError::Signal(libc::SIGILL));
    }

    #[test]
    fn test_varid_check_kill_switch() {
        use tidepool_repr::tree::RecursiveTree;
        use tidepool_repr::types::Literal;
        use tidepool_repr::{CoreFrame, VarId};

        // let v1 = 1 in let v1 = 2 in v1
        let expr = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitInt(1)), // 0
                CoreFrame::Lit(Literal::LitInt(2)), // 1
                CoreFrame::Var(VarId(1)),           // 2
                CoreFrame::LetNonRec {
                    binder: VarId(1),
                    rhs: 1,
                    body: 2,
                }, // 3
                CoreFrame::LetNonRec {
                    binder: VarId(1),
                    rhs: 0,
                    body: 3,
                }, // 4 (root)
            ],
        };
        let table = DataConTable::new();

        // 1. Default ON: must fail
        let res = JitEffectMachine::compile(&expr, &table, 1 << 20);
        assert!(
            matches!(res, Err(JitError::VarIdCollision(_))),
            "Expected VarIdCollision, got success"
        );

        // 2. Kill-switch: must pass
        std::env::set_var("TIDEPOOL_VARID_CHECK", "0");
        let res_disabled = JitEffectMachine::compile(&expr, &table, 1 << 20);
        std::env::remove_var("TIDEPOOL_VARID_CHECK");

        assert!(
            res_disabled.is_ok(),
            "Kill-switch failed to bypass VarId collision: {:?}",
            res_disabled.err()
        );
    }

    /// L7 (repo-review-2026-07-06/01-gc-memory-safety.md, Low findings):
    /// `suspended_continuation` is not a GC root and, before this fix, no
    /// run entry asserted it was `None` — safety rested entirely on
    /// tidepool-repl's external discipline never calling a fresh run entry
    /// on a machine parked at `resume_suspended`. Rather than drive a real
    /// `Ask`-boundary suspension (heavy effect-machine setup), this directly
    /// sets the private field to simulate "already suspended" and confirms
    /// each entry's new `assert!` fires — a clean panic, not silent
    /// corruption of a live continuation.
    #[test]
    fn run_entries_assert_when_a_continuation_is_already_suspended() {
        use tidepool_repr::tree::RecursiveTree;
        use tidepool_repr::types::Literal;
        use tidepool_repr::CoreFrame;

        let expr = RecursiveTree {
            nodes: vec![CoreFrame::Lit(Literal::LitInt(42))],
        };
        let table = DataConTable::new();
        let mut machine = JitEffectMachine::compile_session(&expr, &table, 1 << 16)
            .expect("compile_session failed");
        let func_id = machine.func_id;

        macro_rules! assert_panics_while_suspended {
            ($name:expr, $call:expr) => {
                machine.suspended_continuation = Some(std::ptr::null_mut());
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe($call));
                assert!(
                    result.is_err(),
                    "{} must panic while a continuation is suspended (L7)",
                    $name
                );
                machine.suspended_continuation = None;
            };
        }

        assert_panics_while_suspended!("run_pure", || {
            let _ = machine.run_pure();
        });
        assert_panics_while_suspended!("run_fragment_pure", || {
            let _ = machine.run_fragment_pure(func_id);
        });
        assert_panics_while_suspended!("run_pure_and_bind", || {
            let _ = machine.run_pure_and_bind(func_id);
        });
    }

    // ---------------------------------------------------------------------------
    // Wave 1.A seam tests
    // ---------------------------------------------------------------------------

    /// Build a Con-chain expr + DataConTable that forces >=1 GC under a small nursery.
    /// Mirrors the gc_frame_walker.rs `build_con_chain` + `make_table_with_con` helpers,
    /// and the Con-chain shape of `tidepool_testing::gen::make_gc_forcing_setup`.
    ///
    /// INVARIANT: cfg(test) code in this crate must NOT reference tidepool-testing.
    /// tidepool-testing links its own (non-test) copy of tidepool-codegen, so if this
    /// crate's lib-test binary pulls tidepool-testing in, it ends up linking TWO
    /// copies of tidepool-codegen's `#[no_mangle] extern "C"` host fns (the JIT ABI
    /// requires unmangled names) — a hard duplicate-symbol link error. Integration
    /// tests under tests/*.rs don't have this problem (they're a separate binary that
    /// links tidepool-codegen only once, without `--test` on it), so they're free to
    /// share tidepool_testing::gen::make_gc_forcing_setup; this cfg(test) helper can't.
    fn make_gc_forcing_setup(
        depth: usize,
    ) -> (tidepool_repr::CoreExpr, tidepool_repr::DataConTable) {
        use tidepool_repr::datacon::DataCon;
        use tidepool_repr::types::{DataConId, Literal, VarId};
        use tidepool_repr::{CoreFrame, DataConTable, TreeBuilder};

        let mut bld = TreeBuilder::new();
        let var_x = bld.push(CoreFrame::Var(VarId(0)));
        let g1_rhs = bld.push(CoreFrame::Con {
            tag: DataConId(1),
            fields: vec![var_x],
        });
        let var_g1 = bld.push(CoreFrame::Var(VarId(1)));
        let g2_rhs = bld.push(CoreFrame::Con {
            tag: DataConId(1),
            fields: vec![var_g1],
        });
        let final_con = bld.push(CoreFrame::Con {
            tag: DataConId(1),
            fields: vec![var_x],
        });
        let let_g2 = bld.push(CoreFrame::LetNonRec {
            binder: VarId(2),
            rhs: g2_rhs,
            body: final_con,
        });
        let let_g1 = bld.push(CoreFrame::LetNonRec {
            binder: VarId(1),
            rhs: g1_rhs,
            body: let_g2,
        });
        let lam_x = bld.push(CoreFrame::Lam {
            binder: VarId(0),
            body: let_g1,
        });
        let mut current = bld.push(CoreFrame::Lit(Literal::LitInt(42)));
        for _ in 0..depth {
            let f_var = bld.push(CoreFrame::Var(VarId(99)));
            current = bld.push(CoreFrame::App {
                fun: f_var,
                arg: current,
            });
        }
        bld.push(CoreFrame::LetRec {
            bindings: vec![(VarId(99), lam_x)],
            body: current,
        });
        let expr = bld.build();

        let mut table = DataConTable::new();
        table.insert(DataCon {
            id: DataConId(1),
            name: "C1".to_string(),
            tag: 1,
            rep_arity: 1,
            field_bangs: vec![],
            qualified_name: None,
            type_name: String::new(),
        });
        for (i, kind) in crate::effect_machine::EffContKind::ALL.iter().enumerate() {
            table.insert(DataCon {
                id: DataConId(1000 + i as u64),
                name: kind.name().to_string(),
                tag: (1000 + i) as u32,
                rep_arity: if matches!(
                    kind,
                    crate::effect_machine::EffContKind::Node
                        | crate::effect_machine::EffContKind::Union
                ) {
                    2
                } else {
                    1
                },
                field_bangs: vec![],
                qualified_name: None,
                type_name: String::new(),
            });
        }
        (expr, table)
    }

    /// Test (c): persistent roots survive a RegistryGuard drop; per-run rust
    /// roots and GC state are cleared.
    ///
    /// Bare-VMContext harness (leaf 3's GC cluster reaches exclusively via
    /// `vmctx.machine_state`, never `CURRENT_MACHINE`): owns a `MachineState`,
    /// wires it onto a hand-built `VMContext`, and drives the vmctx-gated
    /// free fns through it — same pattern leaf 1 used for
    /// `set_stack_map_registry`.
    #[test]
    #[serial]
    fn test_persistent_root_survives_guard_drop() {
        let machine_state = MachineState::new();
        let mut vmctx = VMContext {
            alloc_ptr: std::ptr::null_mut(),
            alloc_limit: std::ptr::null_mut(),
            gc_trigger: crate::host_fns::gc_trigger,
            tail_callee: std::ptr::null_mut(),
            tail_arg: std::ptr::null_mut(),
            machine_state: &machine_state as *const MachineState as *mut MachineState,
        };
        let vmctx_ptr = &mut vmctx as *mut VMContext;

        // Register a persistent root (null heap ptr — GC skips null slots)
        let mut persistent_slot: *mut u8 = std::ptr::null_mut();
        unsafe {
            crate::host_fns::register_persistent_root(
                vmctx_ptr,
                &mut persistent_slot as *mut *mut u8,
            );
        }
        assert_eq!(
            unsafe { crate::host_fns::persistent_roots_count(vmctx_ptr) },
            1
        );

        // Register a per-run rust root
        let mut rust_slot: *mut u8 = std::ptr::null_mut();
        unsafe {
            crate::host_fns::register_rust_root(vmctx_ptr, &mut rust_slot as *mut *mut u8);
        }
        assert_eq!(unsafe { crate::host_fns::rust_roots_mark(vmctx_ptr) }, 1);

        // Simulate what RegistryGuard::drop does for the per-run half
        machine_state.clear_run_scratch();

        // Persistent root must survive; rust roots and GC state must be gone
        assert_eq!(
            unsafe { crate::host_fns::persistent_roots_count(vmctx_ptr) },
            1,
            "persistent root must survive clear_run_scratch"
        );
        assert_eq!(
            unsafe { crate::host_fns::rust_roots_mark(vmctx_ptr) },
            0,
            "rust roots must be cleared by clear_run_scratch"
        );
        assert!(
            machine_state.gc_active_range().is_none(),
            "GC state must be cleared by clear_run_scratch"
        );

        // Cleanup
        machine_state.clear_persistent_roots();
    }

    /// Test (d): THE SEAM TEST — compile_session, run, verify heap retention,
    /// verify install re-points, verify persistent root survives second run.
    #[test]
    #[serial]
    fn test_session_heap_seam() {
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                crate::host_fns::reset_test_counters();

                let (expr, table) = make_gc_forcing_setup(40);
                // 2 KiB nursery: forces >=1 GC for the 40-deep chain
                let mut machine = JitEffectMachine::compile_session(&expr, &table, 2048)
                    .expect("compile_session");

                // --- Run 1 ---
                let result1 = machine.run_pure().expect("run 1 should succeed");

                assert!(
                    crate::host_fns::gc_trigger_call_count() > 0,
                    "GC must have fired during run 1 with 2 KiB nursery"
                );
                assert!(
                    machine.session.as_ref().unwrap().heap.is_some(),
                    "session.heap must be Some after GC (heap migrated off nursery)"
                );
                assert!(
                    machine.session.as_ref().unwrap().cursor > 0,
                    "session.cursor must be >0 after run 1"
                );

                // Capture the retained heap ptr BEFORE install takes it
                let retained_heap_ptr = machine
                    .session
                    .as_ref()
                    .unwrap()
                    .heap
                    .as_ref()
                    .unwrap()
                    .as_ptr() as *const u8;

                // install_registries must RE-POINT at the retained buffer (not nursery.start())
                let guard = machine.install_registries();
                let (active_start, _) = machine
                    .machine_state
                    .gc_active_range()
                    .expect("GC state installed");
                assert_eq!(
                    active_start as *const u8, retained_heap_ptr,
                    "install_registries must re-point GC state at the retained heap"
                );
                assert_ne!(
                    active_start as *const u8,
                    machine.nursery.start(),
                    "install must NOT reset to nursery.start()"
                );
                // Drop the guard so reclaim runs and buffer goes back to session
                drop(guard);

                // --- Register a persistent root before run 2 ---
                // Via the machine's own accessor (not the vmctx-gated free fn:
                // there is no live vmctx between runs) — same MachineState
                // cell either way.
                let mut persistent_slot: *mut u8 = std::ptr::null_mut();
                unsafe {
                    machine.register_persistent_root(&mut persistent_slot as *mut *mut u8);
                }
                assert_eq!(machine.persistent_roots_count(), 1);

                // --- Run 2 ---
                let result2 = machine.run_pure().expect("run 2 should succeed");

                // Results must be structurally equivalent (same program, same heap)
                assert_eq!(
                    format!("{:?}", result1),
                    format!("{:?}", result2),
                    "second run must produce the same value"
                );

                // Persistent root must have survived run 2's teardown
                assert_eq!(
                    machine.persistent_roots_count(),
                    1,
                    "persistent root must survive run-2 teardown (clear_run_scratch)"
                );

                // Drop the machine: free_session_heap clears persistent roots.
                // Per-machine ownership means this is now structural — the
                // MachineState (and its persistent_roots Vec) is deallocated
                // with `machine`, so there is nothing left to query; the
                // assertion this replaces (`persistent_roots_count() == 0`
                // read through a since-freed handle) is no longer expressible
                // and would be UB, not a check.
                drop(machine);
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
