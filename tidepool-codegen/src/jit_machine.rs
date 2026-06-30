use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cranelift_module::FuncId;
use tidepool_effect::{DispatchEffect, EffectContext, EffectError};
use tidepool_eval::value::Value;
use tidepool_repr::{CoreExpr, DataConTable};

use crate::context::VMContext;
use crate::effect_machine::{CompiledEffectMachine, ConTags};
use crate::heap_bridge;
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
    #[error("JIT signal during heap bridge: {0}")]
    Signal(#[from] crate::signal_safety::SignalError),
    #[error("Effect handler response too large ({nodes} value nodes, max {limit}). Narrow your query to return fewer results.")]
    EffectResponseTooLarge { nodes: usize, limit: usize },
    #[error("VarId collision at load: {0}. This indicates a Haskell-side VarId-scheme regression; set TIDEPOOL_VARID_CHECK=0 only to bypass for bisection.")]
    VarIdCollision(#[from] tidepool_repr::VarIdCollision),
}

/// Kill-switch for the load-time duplicate-VarId check (#313 defense).
/// Default ON; `TIDEPOOL_VARID_CHECK=0` disables it (bisection escape hatch).
fn varid_check_enabled() -> bool {
    std::env::var("TIDEPOOL_VARID_CHECK").map_or(true, |v| v != "0")
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
    /// External cancellation flag. The JIT installs a thread-local clone of this
    /// `Arc` via `set_cancel_flag` before entering compiled code; the next
    /// GC safepoint observes the flag and aborts execution with
    /// `YieldError::Cancelled` if it has been set. See [`Self::cancel_handle`].
    cancel_flag: Arc<AtomicBool>,
    /// Session state for GHCi-style persistent machines (Wave 1.A).
    /// `None` for one-shot machines created by [`Self::compile`].
    session: Option<SessionState>,
}

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
/// the machine's `Nursery` into a `Vec<u8>` owned here. `cursor` is the
/// bump high-water mark (bytes from the start of `heap`, or from
/// `nursery.start()` when `heap` is None) at the end of the last run —
/// the next run resumes allocation from there.
struct SessionState {
    heap: Option<Vec<u8>>,
    cursor: usize,
    // Wave 1.A (Worker-Tenure): populated at bind time; scaffold only until tenure lands.
    #[allow(dead_code)]
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
    reclaim: Option<(*mut Option<SessionState>, *const crate::context::VMContext)>,
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
            self.reclaim = Some((session, vmctx));
        }
    }
}

impl Drop for RegistryGuard {
    fn drop(&mut self) {
        // Reclaim the live heap buffer back onto the machine BEFORE
        // clear_run_scratch takes GcState (which would free active_buffer).
        if let Some((sess, vmctx)) = self.reclaim {
            // SAFETY: vmctx points into the enclosing run/run_pure stack
            // frame which is still live. VMContext has no custom Drop so its
            // bytes are intact even after the value is logically dropped.
            // sess points to JitEffectMachine::session in the same frame.
            unsafe {
                let ap = (*vmctx).alloc_ptr;
                let (buf, cur) = crate::host_fns::reclaim_session_heap(ap);
                if let Some(s) = (*sess).as_mut() {
                    s.heap = buf;
                    s.cursor = cur;
                }
            }
        }
        crate::host_fns::clear_run_scratch();
        crate::host_fns::clear_stack_map_registry();
        crate::host_fns::clear_cancel_flag();
        crate::host_fns::clear_parked_streams();
        crate::debug::clear_lambda_registry();
        // Clean up remaining thread-local state for same-thread reuse
        let _ = crate::host_fns::take_runtime_error();
        let _ = crate::host_fns::drain_diagnostics();
        crate::host_fns::reset_call_depth();
        crate::host_fns::set_exec_context("");
    }
}

/// The compiled artifacts produced by [`JitEffectMachine::compile_inner`],
/// shared by the one-shot (`compile`) and session (`compile_session`) ctors.
type CompiledParts = (
    CodegenPipeline,
    Nursery,
    Result<ConTags, &'static str>,
    FuncId,
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
        let expr = crate::datacon_env::wrap_with_datacon_env(expr, table);
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
        Ok((pipeline, nursery, tags, func_id))
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
        let (pipeline, nursery, tags, func_id) = Self::compile_inner(expr, table, nursery_size)?;
        Ok(Self {
            pipeline,
            nursery,
            tags,
            func_id,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            session: None,
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
        let (pipeline, nursery, tags, func_id) = Self::compile_inner(expr, table, nursery_size)?;
        Ok(Self {
            pipeline,
            nursery,
            tags,
            func_id,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            session: Some(SessionState {
                heap: None,
                cursor: 0,
                old_space: crate::old_space::OldSpace::new(),
            }),
        })
    }

    /// Obtain a clone-able, thread-safe handle for requesting cancellation of
    /// this machine's next (or in-flight) run. The handle remains valid for
    /// the lifetime of the machine; multiple handles may be held concurrently.
    pub fn cancel_handle(&self) -> CancelHandle {
        CancelHandle(self.cancel_flag.clone())
    }

    /// Install per-run thread-local registries and return a drop guard.
    ///
    /// For session machines: re-points the GC state at the retained heap
    /// buffer (if a GC has already run) OR at `nursery.start()` (first run
    /// only). For one-shot machines: always points at `nursery.start()`.
    pub(crate) fn install_registries(&mut self) -> RegistryGuard {
        crate::debug::set_lambda_registry(self.pipeline.build_lambda_registry());
        crate::host_fns::set_stack_map_registry(&self.pipeline.stack_maps);
        match &mut self.session {
            Some(s) => match s.heap.take() {
                Some(buf) => crate::host_fns::install_session_buffer(buf),
                None => crate::host_fns::set_gc_state(
                    self.nursery.start() as *mut u8,
                    self.nursery.size(),
                ),
            },
            None => {
                crate::host_fns::set_gc_state(self.nursery.start() as *mut u8, self.nursery.size())
            }
        }
        crate::host_fns::set_cancel_flag(self.cancel_flag.clone());
        RegistryGuard {
            is_session: self.session.is_some(),
            reclaim: None,
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
        let (start, size) = crate::host_fns::gc_active_range()
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
        // Arm reclaim so Drop can recover active_buffer → session.heap.
        // SAFETY: machine.vmctx_mut() points into `machine` on this stack frame;
        // CompiledEffectMachine has no custom Drop so the bytes are valid when
        // _guard drops (machine drops first but the stack frame is still live).
        unsafe {
            _guard.arm_reclaim(&mut self.session as *mut _, machine.vmctx_mut() as *const _);
        }
        crate::host_fns::reset_call_depth();
        crate::host_fns::set_exec_context("stepping main function");
        // SAFETY: with_signal_protection wraps the JIT call with sigsetjmp for crash recovery.
        // machine.step() calls the JIT function through a valid function pointer.
        let mut yield_result =
            match unsafe { crate::signal_safety::with_signal_protection(|| machine.step()) } {
                Ok(y) => y,
                Err(e) => signal_error_to_yield(e),
            };

        let result = loop {
            match yield_result {
                Yield::Done(ptr) => {
                    // SAFETY: ptr is a valid heap pointer returned by the JIT. vmctx_ptr is
                    // valid for forcing thunks. Signal protection guards against crashes.
                    let bridge_res = unsafe {
                        let vmctx_ptr = machine.vmctx_mut() as *mut VMContext;
                        crate::signal_safety::with_signal_protection(|| {
                            heap_bridge::heap_to_value_forcing(ptr, vmctx_ptr)
                        })
                    }
                    .map_err(JitError::Signal)?;
                    // Forcing may have triggered a `gc_trigger` cancel
                    // observation; prefer that over a symptomatic bridge
                    // error (see the corresponding comment in `run_pure`).
                    if let Some(err) = crate::host_fns::take_runtime_error() {
                        break Err(JitError::Yield(crate::yield_type::YieldError::from(err)));
                    }
                    let val = bridge_res.map_err(JitError::HeapBridge)?;
                    break Ok(val);
                }
                Yield::Request {
                    tag,
                    request,
                    continuation,
                } => {
                    // SAFETY: request is a valid heap pointer from the JIT effect dispatch.
                    let bridge_res = unsafe {
                        let vmctx_ptr = machine.vmctx_mut() as *mut VMContext;
                        crate::signal_safety::with_signal_protection(|| {
                            heap_bridge::heap_to_value_forcing(request, vmctx_ptr)
                        })
                    }
                    .map_err(JitError::Signal)?;
                    if let Some(err) = crate::host_fns::take_runtime_error() {
                        break Err(JitError::Yield(crate::yield_type::YieldError::from(err)));
                    }
                    let req_val = bridge_res.map_err(JitError::HeapBridge)?;
                    log::debug!(target: "tidepool::effects", "effect tag={} request={:?}", tag, req_val);
                    let cx = EffectContext::with_user(table, user);
                    let response = handlers.dispatch(tag, &req_val, &cx)?;

                    // External cancellation safepoint at the effect-dispatch
                    // boundary. The handler we just called may itself have
                    // flipped the cancel flag (a watchdog handler is the
                    // canonical case); the JIT-internal safepoints
                    // (gc_trigger, trampoline_resolve) only fire on
                    // tail-recursive or heavy-allocating Haskell, so
                    // freer-simple effect loops would otherwise observe
                    // the cancel only as an eventual unrelated error.
                    // Checking here gives prompt unwind for the realistic
                    // handler-driven scenario without depending on the
                    // shape of the compiled program.
                    if self.cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
                        break Err(JitError::Yield(crate::yield_type::YieldError::Runtime(
                            crate::host_fns::RuntimeError::Cancelled,
                        )));
                    }

                    // Response materialization. Two channels:
                    //
                    // Stream: the handler parked nothing and built nothing —
                    // elements convert per-pull, chunk-by-chunk, as Haskell
                    // forces tails (`take k` of a huge listing converts ~one
                    // chunk; an infinite producer is a legitimate infinite
                    // list). With the TIDEPOOL_LAZY_RESULTS=0 kill-switch the
                    // stream drains eagerly through the node cap instead.
                    //
                    // Complete: classic Value. Long list spines are flattened
                    // BY VALUE (iterative dismantle) and re-parked as a
                    // pre-converted stream — a deep spine must never reach a
                    // recursive Drop or recursive value_to_heap (~3 stack
                    // frames per cell overflow the eval thread; the fault
                    // lands outside signal protection and silently kills the
                    // thread — see .tidepool/crash.log). The node cap remains
                    // as a backstop for large non-list responses.
                    const LAZY_SPINE_THRESHOLD_NODES: usize = 2_000;
                    const MAX_EFFECT_RESPONSE_NODES: usize = 100_000;
                    let lazy_enabled = std::env::var("TIDEPOOL_LAZY_RESULTS")
                        .map(|v| v != "0")
                        .unwrap_or(true);

                    // Normalize both channels to one of: a stream to park, a
                    // Value to convert eagerly, or an already-materialized
                    // heap pointer (kill-switch drains).
                    enum Plan {
                        Park(crate::host_fns::ParkedStream),
                        Eager(tidepool_eval::value::Value),
                        Ready(*mut u8),
                    }
                    let plan = match response {
                        tidepool_effect::Response::Stream(s) => {
                            let (mut source, cons_id, nil_id) = s.into_parts();
                            if lazy_enabled {
                                Plan::Park(crate::host_fns::ParkedStream {
                                    source,
                                    cons_tag: cons_id.0,
                                    nil_tag: nil_id.0,
                                    table: table.clone(),
                                })
                            } else {
                                // Kill-switch: drain through the node cap.
                                // (This makes infinite producers a clean
                                // TooLarge error instead of divergence.)
                                let mut items = Vec::new();
                                let mut nodes = 0usize;
                                let mut too_large = false;
                                while let Some(r) = source.next_value(table) {
                                    let v =
                                        r.map_err(|e| JitError::from(EffectError::Bridge(e)))?;
                                    nodes += 3 + v.node_count();
                                    items.push(v);
                                    if nodes > MAX_EFFECT_RESPONSE_NODES {
                                        too_large = true;
                                        break;
                                    }
                                }
                                if too_large {
                                    break Err(JitError::EffectResponseTooLarge {
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
                                    break Err(JitError::Yield(
                                        crate::yield_type::YieldError::from(err),
                                    ));
                                }
                                Plan::Ready(p)
                            }
                        }
                        tidepool_effect::Response::Complete(resp_val) => {
                            let spine = probe_list_spine(&resp_val)
                                .filter(|&(_, _, len)| len > LAZY_SPINE_THRESHOLD_NODES);
                            match spine {
                                Some((cons_tag, nil_tag, len)) if lazy_enabled => {
                                    // Re-park the dismantled spine as a
                                    // pre-converted stream: one registry, one
                                    // chunk materializer for both channels.
                                    let items = dismantle_list_spine(resp_val, len);
                                    Plan::Park(crate::host_fns::ParkedStream {
                                        source: Box::new(crate::host_fns::ReadySource::new(items)),
                                        cons_tag,
                                        nil_tag,
                                        // Pre-converted: table never consulted.
                                        table: tidepool_repr::DataConTable::new(),
                                    })
                                }
                                Some((cons_tag, nil_tag, len)) => {
                                    // Kill-switch: eager iterative
                                    // materialization, cap still applies.
                                    let items = dismantle_list_spine(resp_val, len);
                                    let nodes = 3 * len
                                        + items.iter().map(|v| v.node_count()).sum::<usize>();
                                    if nodes > MAX_EFFECT_RESPONSE_NODES {
                                        break Err(JitError::EffectResponseTooLarge {
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
                                        break Err(JitError::Yield(
                                            crate::yield_type::YieldError::from(err),
                                        ));
                                    }
                                    Plan::Ready(p)
                                }
                                None => Plan::Eager(resp_val),
                            }
                        }
                    };
                    let resp_ptr = match plan {
                        Plan::Ready(p) => p,
                        Plan::Park(stream) => {
                            let id = crate::host_fns::park_stream(stream);
                            // SAFETY: vmctx is valid with installed GC state.
                            let p = unsafe {
                                crate::signal_safety::with_signal_protection(|| {
                                    crate::host_fns::alloc_stream_tail_thunk(
                                        machine.vmctx_mut(),
                                        id,
                                        0,
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
                        Plan::Eager(resp_val) => {
                            let nodes = resp_val.node_count();
                            if nodes > MAX_EFFECT_RESPONSE_NODES {
                                break Err(JitError::EffectResponseTooLarge {
                                    nodes,
                                    limit: MAX_EFFECT_RESPONSE_NODES,
                                });
                            }
                            // SAFETY: Converting a Value back to a heap object
                            // in the nursery. vmctx has sufficient nursery
                            // space (GC may have reclaimed).
                            unsafe {
                                crate::signal_safety::with_signal_protection(|| {
                                    heap_bridge::value_to_heap(&resp_val, machine.vmctx_mut())
                                })
                            }
                            .map_err(JitError::Signal)?
                            .map_err(JitError::HeapBridge)?
                        }
                    };
                    crate::host_fns::reset_call_depth();
                    crate::host_fns::set_exec_context(&format!(
                        "resuming after effect tag={}",
                        tag
                    ));
                    // SAFETY: continuation and resp_ptr are valid nursery heap pointers.
                    // resume applies the continuation tree to the response.
                    yield_result = match unsafe {
                        crate::signal_safety::with_signal_protection(|| {
                            machine.resume(continuation, resp_ptr)
                        })
                    } {
                        Ok(y) => y,
                        Err(e) => signal_error_to_yield(e),
                    };
                }
                Yield::Error(e) => break Err(JitError::Yield(e)),
            }
        };

        result
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
        // Arm reclaim so Drop can recover active_buffer → session.heap.
        // SAFETY: &vmctx lives on this stack frame; VMContext has no custom Drop
        // so its bytes are valid when _guard drops (which is before run_pure returns).
        unsafe {
            _guard.arm_reclaim(&mut self.session as *mut _, &vmctx as *const _);
        }

        crate::host_fns::reset_call_depth();
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

        // Re-check for runtime errors recorded during thunk forcing. The
        // bridge calls back into JIT via `heap_force`, which can trigger
        // `gc_trigger` — and an external cancel observed there sets
        // `RuntimeError::Cancelled` but surfaces only as a bridge-level
        // `UnevaluatedThunk` failure (the forced thunk never completed).
        // Prefer the cancellation cause over the symptomatic bridge error.
        if let Some(err) = crate::host_fns::take_runtime_error() {
            return Err(JitError::Yield(crate::yield_type::YieldError::from(err)));
        }
        bridge_result.map_err(JitError::HeapBridge)
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
        // Mirror compile_inner's tree shaping so the fragment is emitted exactly
        // like the original entry; only the JITModule destination differs (it is
        // already finalized — we add a fresh round).
        let expr = tidepool_repr::normalize(expr, table);
        let expr = crate::datacon_env::wrap_with_datacon_env(expr, table);
        // Boxed-literal wrapper tolerance is per-compile; refresh from this
        // fragment's table (see compile_inner). Runtime-inert — read only during
        // emission — so refreshing it does not perturb already-compiled code.
        self.pipeline.lit_wrappers = crate::emit::LitWrapperIds::from_table(table);
        let func_id =
            crate::emit::expr::compile_expr(&mut self.pipeline, &expr, name, external_env)
                .map_err(JitError::Compilation)?;
        // Multi-round finalize: finalize_definitions is safe to re-run; finalize()
        // drains only THIS round's pending stack maps and appends them to the
        // registry (round-1 maps were drained on the first finalize).
        self.pipeline.finalize()?;
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
        // Per-thread signal handler + altstack; see `run`. Idempotent.
        crate::signal_safety::install();

        let mut _guard = self.install_registries();

        // SAFETY: finalized JIT code pointer; calling convention per contract.
        let func_ptr: unsafe extern "C" fn(*mut VMContext) -> *mut u8 =
            unsafe { std::mem::transmute(self.pipeline.get_function_ptr(func_id)) };
        let mut vmctx = self.make_session_vmctx();

        crate::host_fns::reset_call_depth();
        crate::host_fns::set_exec_context("running pure computation (bind)");
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
        let from = crate::host_fns::gc_active_range().expect("GC state installed for the bind run");
        let from_range = (from.0 as *const u8, unsafe {
            from.0.add(from.1) as *const u8
        });
        // SAFETY: nf_ptr is a live heap object inside the nursery from-range;
        // tenure evacuates its closure and registers the returned slot as a
        // persistent root valid for the machine's life.
        let slot = unsafe {
            self.session
                .as_mut()
                .expect("session machine")
                .old_space
                .tenure(nf_ptr, from_range)
        };

        // Arm reclaim LAST (after all `self.session` access) so the guard's raw
        // pointer to `self.session` is not aliased by an intervening `&mut`
        // borrow. On drop the guard recovers the live buffer + high-water cursor
        // → session.heap/cursor for the next run. SAFETY: &vmctx lives on this
        // frame; VMContext has no custom Drop so its bytes are valid at drop.
        unsafe {
            _guard.arm_reclaim(&mut self.session as *mut _, &vmctx as *const _);
        }
        Ok(slot)
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
        // NOTE: do NOT arm reclaim before the step loop — the Done arm accesses
        // self.session (for tenure) and arm_reclaim stores a raw *mut self.session;
        // the two cannot alias. Follow run_pure_and_bind's ordering: tenure first
        // inside the loop, arm_reclaim LAST after the loop exits.

        crate::host_fns::reset_call_depth();
        crate::host_fns::set_exec_context("stepping effectful computation (bind)");
        // SAFETY: with_signal_protection wraps the JIT call with sigsetjmp for crash recovery.
        // machine.step() calls the JIT function through a valid function pointer.
        let mut yield_result =
            match unsafe { crate::signal_safety::with_signal_protection(|| machine.step()) } {
                Ok(y) => y,
                Err(e) => signal_error_to_yield(e),
            };

        let result = loop {
            match yield_result {
                Yield::Done(ptr) => {
                    if let Some(err) = crate::host_fns::take_runtime_error() {
                        break Err(JitError::Yield(crate::yield_type::YieldError::from(err)));
                    }
                    if ptr.is_null() {
                        break Err(JitError::Yield(crate::yield_type::YieldError::NullPointer));
                    }

                    // K — optionally deep-force to NF before tenuring (Tier0).
                    // Tier1 closures are NOT forced (they are callable code, not data).
                    // SAFETY: ptr is a valid heap object; machine.vmctx_mut() is the
                    // active VMContext for forcing thunks.
                    let nf_ptr = if forced {
                        let force_res = unsafe {
                            crate::signal_safety::with_signal_protection(|| {
                                crate::host_fns::deep_force(
                                    machine.vmctx_mut() as *mut VMContext,
                                    ptr,
                                )
                            })
                        };
                        let nf = match force_res {
                            Err(e) => break Err(JitError::Signal(e)),
                            Ok(p) => p,
                        };
                        // Forcing may have triggered a gc_trigger cancel observation;
                        // prefer that over a symptomatic bridge error.
                        if let Some(err) = crate::host_fns::take_runtime_error() {
                            break Err(JitError::Yield(crate::yield_type::YieldError::from(err)));
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
                    let from = crate::host_fns::gc_active_range()
                        .expect("GC state installed for the bind run");
                    let from_range = (from.0 as *const u8, unsafe {
                        from.0.add(from.1) as *const u8
                    });
                    // SAFETY: nf_ptr is a live heap object inside the nursery from-range;
                    // tenure evacuates its closure and registers the returned slot as a
                    // persistent root valid for the machine's life.
                    let slot = unsafe {
                        self.session
                            .as_mut()
                            .expect("session machine")
                            .old_space
                            .tenure(nf_ptr, from_range)
                    };
                    break Ok(slot);
                }
                Yield::Request {
                    tag,
                    request,
                    continuation,
                } => {
                    // SAFETY: request is a valid heap pointer from the JIT effect dispatch.
                    let bridge_res = unsafe {
                        let vmctx_ptr = machine.vmctx_mut() as *mut VMContext;
                        crate::signal_safety::with_signal_protection(|| {
                            heap_bridge::heap_to_value_forcing(request, vmctx_ptr)
                        })
                    }
                    .map_err(JitError::Signal)?;
                    if let Some(err) = crate::host_fns::take_runtime_error() {
                        break Err(JitError::Yield(crate::yield_type::YieldError::from(err)));
                    }
                    let req_val = bridge_res.map_err(JitError::HeapBridge)?;
                    log::debug!(target: "tidepool::effects", "effect tag={} request={:?}", tag, req_val);
                    let cx = EffectContext::with_user(table, user);
                    let response = handlers.dispatch(tag, &req_val, &cx)?;

                    // External cancellation safepoint at the effect-dispatch boundary.
                    if self.cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
                        break Err(JitError::Yield(crate::yield_type::YieldError::Runtime(
                            crate::host_fns::RuntimeError::Cancelled,
                        )));
                    }

                    const LAZY_SPINE_THRESHOLD_NODES: usize = 2_000;
                    const MAX_EFFECT_RESPONSE_NODES: usize = 100_000;

                    let lazy_enabled = std::env::var("TIDEPOOL_LAZY_RESULTS")
                        .map(|v| v != "0")
                        .unwrap_or(true);

                    enum Plan {
                        Park(crate::host_fns::ParkedStream),
                        Eager(tidepool_eval::value::Value),
                        Ready(*mut u8),
                    }
                    let plan = match response {
                        tidepool_effect::Response::Stream(s) => {
                            let (mut source, cons_id, nil_id) = s.into_parts();
                            if lazy_enabled {
                                Plan::Park(crate::host_fns::ParkedStream {
                                    source,
                                    cons_tag: cons_id.0,
                                    nil_tag: nil_id.0,
                                    table: table.clone(),
                                })
                            } else {
                                let mut items = Vec::new();
                                let mut nodes = 0usize;
                                let mut too_large = false;
                                while let Some(r) = source.next_value(table) {
                                    let v =
                                        r.map_err(|e| JitError::from(EffectError::Bridge(e)))?;
                                    nodes += 3 + v.node_count();
                                    items.push(v);
                                    if nodes > MAX_EFFECT_RESPONSE_NODES {
                                        too_large = true;
                                        break;
                                    }
                                }
                                if too_large {
                                    break Err(JitError::EffectResponseTooLarge {
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
                                    break Err(JitError::Yield(
                                        crate::yield_type::YieldError::from(err),
                                    ));
                                }
                                Plan::Ready(p)
                            }
                        }
                        tidepool_effect::Response::Complete(resp_val) => {
                            let spine = probe_list_spine(&resp_val)
                                .filter(|&(_, _, len)| len > LAZY_SPINE_THRESHOLD_NODES);
                            match spine {
                                Some((cons_tag, nil_tag, len)) if lazy_enabled => {
                                    let items = dismantle_list_spine(resp_val, len);
                                    Plan::Park(crate::host_fns::ParkedStream {
                                        source: Box::new(crate::host_fns::ReadySource::new(items)),
                                        cons_tag,
                                        nil_tag,
                                        table: tidepool_repr::DataConTable::new(),
                                    })
                                }
                                Some((cons_tag, nil_tag, len)) => {
                                    let items = dismantle_list_spine(resp_val, len);
                                    let nodes = 3 * len
                                        + items.iter().map(|v| v.node_count()).sum::<usize>();
                                    if nodes > MAX_EFFECT_RESPONSE_NODES {
                                        break Err(JitError::EffectResponseTooLarge {
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
                                        break Err(JitError::Yield(
                                            crate::yield_type::YieldError::from(err),
                                        ));
                                    }
                                    Plan::Ready(p)
                                }
                                None => Plan::Eager(resp_val),
                            }
                        }
                    };
                    let resp_ptr = match plan {
                        Plan::Ready(p) => p,
                        Plan::Park(stream) => {
                            let id = crate::host_fns::park_stream(stream);
                            // SAFETY: vmctx is valid with installed GC state.
                            let p = unsafe {
                                crate::signal_safety::with_signal_protection(|| {
                                    crate::host_fns::alloc_stream_tail_thunk(
                                        machine.vmctx_mut(),
                                        id,
                                        0,
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
                        Plan::Eager(resp_val) => {
                            let nodes = resp_val.node_count();
                            if nodes > MAX_EFFECT_RESPONSE_NODES {
                                break Err(JitError::EffectResponseTooLarge {
                                    nodes,
                                    limit: MAX_EFFECT_RESPONSE_NODES,
                                });
                            }
                            // SAFETY: Converting a Value back to a heap object in the
                            // nursery. vmctx has sufficient nursery space (GC may have
                            // reclaimed).
                            unsafe {
                                crate::signal_safety::with_signal_protection(|| {
                                    heap_bridge::value_to_heap(&resp_val, machine.vmctx_mut())
                                })
                            }
                            .map_err(JitError::Signal)?
                            .map_err(JitError::HeapBridge)?
                        }
                    };
                    crate::host_fns::reset_call_depth();
                    crate::host_fns::set_exec_context(&format!(
                        "resuming after effect tag={}",
                        tag
                    ));
                    // SAFETY: continuation and resp_ptr are valid nursery heap pointers.
                    // resume applies the continuation tree to the response.
                    yield_result = match unsafe {
                        crate::signal_safety::with_signal_protection(|| {
                            machine.resume(continuation, resp_ptr)
                        })
                    } {
                        Ok(y) => y,
                        Err(e) => signal_error_to_yield(e),
                    };
                }
                Yield::Error(e) => break Err(JitError::Yield(e)),
            }
        };

        // Arm reclaim LAST (after all `self.session` access — tenure is in the
        // Done arm above) so the guard's raw pointer to `self.session` is not
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

        let tags = self.tags.map_err(JitError::MissingConTags)?;

        crate::signal_safety::install();
        let mut _guard = self.install_registries();

        let func_ptr: unsafe extern "C" fn(*mut VMContext) -> *mut u8 =
            unsafe { std::mem::transmute(self.pipeline.get_function_ptr(func_id)) };
        let vmctx = self.make_session_vmctx();
        let mut machine = CompiledEffectMachine::new(func_ptr, vmctx, tags);
        // NOTE: do NOT arm reclaim before the step loop (same ordering as
        // run_fragment_and_bind — tenure is in the Done arm).

        crate::host_fns::reset_call_depth();
        crate::host_fns::set_exec_context("stepping effectful computation (multi-bind)");
        let mut yield_result =
            match unsafe { crate::signal_safety::with_signal_protection(|| machine.step()) } {
                Ok(y) => y,
                Err(e) => signal_error_to_yield(e),
            };

        let result = loop {
            match yield_result {
                Yield::Done(tuple_ptr) => {
                    if let Some(err) = crate::host_fns::take_runtime_error() {
                        break Err(JitError::Yield(crate::yield_type::YieldError::from(err)));
                    }
                    if tuple_ptr.is_null() {
                        break Err(JitError::Yield(crate::yield_type::YieldError::NullPointer));
                    }

                    // GC-safe projection protocol:
                    // 1. deep_force the WHOLE TUPLE first. deep_force internally
                    //    registers every pending parent as a Rust GC root and
                    //    re-reads field slots from the live (possibly relocated)
                    //    parent after each heap_force — so no pointer is cached
                    //    across a GC. Returns nf_tuple: the post-GC NF address with
                    //    all field slots updated to live NF children.
                    //    Closures (TAG_CLOSURE) are forced to WHNF and left as-is.
                    let nf_tuple = unsafe {
                        crate::signal_safety::with_signal_protection(|| {
                            crate::host_fns::deep_force(
                                machine.vmctx_mut() as *mut VMContext,
                                tuple_ptr,
                            )
                        })
                    }
                    .map_err(JitError::Signal)?;
                    if let Some(err) = crate::host_fns::take_runtime_error() {
                        break Err(JitError::Yield(crate::yield_type::YieldError::from(err)));
                    }

                    // 2. Validate arity from the NF (post-GC) object.
                    let n_actual = unsafe {
                        *(nf_tuple.add(crate::layout::CON_NUM_FIELDS_OFFSET as usize) as *const u16)
                            as usize
                    };
                    if n_actual != n_fields {
                        break Err(JitError::Yield(crate::yield_type::YieldError::Runtime(
                            crate::host_fns::RuntimeError::UserErrorMsg(format!(
                                "multi-bind: result tuple has {} fields, expected {}",
                                n_actual, n_fields
                            )),
                        )));
                    }

                    // 3. Capture from_range AFTER deep_force (GC may have changed
                    //    the active region). tenure() is pure Rust — no JIT GC
                    //    fires — so this range stays valid for all field tenures.
                    let from = crate::host_fns::gc_active_range()
                        .expect("GC state installed for the bind run");
                    let from_range = (from.0 as *const u8, unsafe {
                        from.0.add(from.1) as *const u8
                    });

                    // 4. Project each field from nf_tuple and tenure.
                    //    nf_tuple stays valid across all tenure() calls (no JIT GC).
                    //    deep_force already wrote live NF pointers into each slot.
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
                                .tenure(field_ptr, from_range)
                        };
                        slots.push(slot);
                    }
                    break Ok(slots);
                }
                Yield::Request {
                    tag,
                    request,
                    continuation,
                } => {
                    let bridge_res = unsafe {
                        let vmctx_ptr = machine.vmctx_mut() as *mut VMContext;
                        crate::signal_safety::with_signal_protection(|| {
                            heap_bridge::heap_to_value_forcing(request, vmctx_ptr)
                        })
                    }
                    .map_err(JitError::Signal)?;
                    if let Some(err) = crate::host_fns::take_runtime_error() {
                        break Err(JitError::Yield(crate::yield_type::YieldError::from(err)));
                    }
                    let req_val = bridge_res.map_err(JitError::HeapBridge)?;
                    log::debug!(target: "tidepool::effects", "effect tag={} request={:?}", tag, req_val);
                    let cx = EffectContext::with_user(table, user);
                    let response = handlers.dispatch(tag, &req_val, &cx)?;

                    if self.cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
                        break Err(JitError::Yield(crate::yield_type::YieldError::Runtime(
                            crate::host_fns::RuntimeError::Cancelled,
                        )));
                    }

                    const LAZY_SPINE_THRESHOLD_NODES: usize = 2_000;
                    const MAX_EFFECT_RESPONSE_NODES: usize = 100_000;
                    let lazy_enabled = std::env::var("TIDEPOOL_LAZY_RESULTS")
                        .map(|v| v != "0")
                        .unwrap_or(true);

                    // Distinct name to avoid shadowing the Plan enum in run_fragment_and_bind.
                    enum MultibindPlan {
                        Park(crate::host_fns::ParkedStream),
                        Eager(tidepool_eval::value::Value),
                        Ready(*mut u8),
                    }
                    let plan = match response {
                        tidepool_effect::Response::Stream(s) => {
                            let (mut source, cons_id, nil_id) = s.into_parts();
                            if lazy_enabled {
                                MultibindPlan::Park(crate::host_fns::ParkedStream {
                                    source,
                                    cons_tag: cons_id.0,
                                    nil_tag: nil_id.0,
                                    table: table.clone(),
                                })
                            } else {
                                let mut items = Vec::new();
                                let mut nodes = 0usize;
                                let mut too_large = false;
                                while let Some(r) = source.next_value(table) {
                                    let v =
                                        r.map_err(|e| JitError::from(EffectError::Bridge(e)))?;
                                    nodes += 3 + v.node_count();
                                    items.push(v);
                                    if nodes > MAX_EFFECT_RESPONSE_NODES {
                                        too_large = true;
                                        break;
                                    }
                                }
                                if too_large {
                                    break Err(JitError::EffectResponseTooLarge {
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
                                    break Err(JitError::Yield(
                                        crate::yield_type::YieldError::from(err),
                                    ));
                                }
                                MultibindPlan::Ready(p)
                            }
                        }
                        tidepool_effect::Response::Complete(resp_val) => {
                            let spine = probe_list_spine(&resp_val)
                                .filter(|&(_, _, len)| len > LAZY_SPINE_THRESHOLD_NODES);
                            match spine {
                                Some((cons_tag, nil_tag, len)) if lazy_enabled => {
                                    let items = dismantle_list_spine(resp_val, len);
                                    MultibindPlan::Park(crate::host_fns::ParkedStream {
                                        source: Box::new(crate::host_fns::ReadySource::new(items)),
                                        cons_tag,
                                        nil_tag,
                                        table: tidepool_repr::DataConTable::new(),
                                    })
                                }
                                Some((cons_tag, nil_tag, len)) => {
                                    let items = dismantle_list_spine(resp_val, len);
                                    let nodes = 3 * len
                                        + items.iter().map(|v| v.node_count()).sum::<usize>();
                                    if nodes > MAX_EFFECT_RESPONSE_NODES {
                                        break Err(JitError::EffectResponseTooLarge {
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
                                        break Err(JitError::Yield(
                                            crate::yield_type::YieldError::from(err),
                                        ));
                                    }
                                    MultibindPlan::Ready(p)
                                }
                                None => MultibindPlan::Eager(resp_val),
                            }
                        }
                    };
                    let resp_ptr = match plan {
                        MultibindPlan::Ready(p) => p,
                        MultibindPlan::Park(stream) => {
                            let id = crate::host_fns::park_stream(stream);
                            let p = unsafe {
                                crate::signal_safety::with_signal_protection(|| {
                                    crate::host_fns::alloc_stream_tail_thunk(
                                        machine.vmctx_mut(),
                                        id,
                                        0,
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
                        MultibindPlan::Eager(resp_val) => {
                            let nodes = resp_val.node_count();
                            if nodes > MAX_EFFECT_RESPONSE_NODES {
                                break Err(JitError::EffectResponseTooLarge {
                                    nodes,
                                    limit: MAX_EFFECT_RESPONSE_NODES,
                                });
                            }
                            unsafe {
                                crate::signal_safety::with_signal_protection(|| {
                                    heap_bridge::value_to_heap(&resp_val, machine.vmctx_mut())
                                })
                            }
                            .map_err(JitError::Signal)?
                            .map_err(JitError::HeapBridge)?
                        }
                    };
                    crate::host_fns::reset_call_depth();
                    crate::host_fns::set_exec_context(&format!(
                        "resuming after effect tag={} (multi-bind)",
                        tag
                    ));
                    yield_result = match unsafe {
                        crate::signal_safety::with_signal_protection(|| {
                            machine.resume(continuation, resp_ptr)
                        })
                    } {
                        Ok(y) => y,
                        Err(e) => signal_error_to_yield(e),
                    };
                }
                Yield::Error(e) => break Err(JitError::Yield(e)),
            }
        };

        // Arm reclaim LAST (after all self.session access — tenure is in the
        // Done arm above). Same UAF ordering as run_fragment_and_bind.
        unsafe {
            _guard.arm_reclaim(&mut self.session as *mut _, machine.vmctx_mut() as *const _);
        }
        result
    }

    /// Register a session-scoped GC root slot that survives across runs (i.e.
    /// across `RegistryGuard` drops), unlike the per-run `RUST_ROOTS`.
    ///
    /// Wave 1.A (component D): fill the `PERSISTENT_ROOTS` thread-local so a
    /// tenured binding's root is appended to `perform_gc`'s root set and is NOT
    /// cleared by the per-run `clear_run_scratch`. Takes a slot pointer
    /// (`*mut *mut u8`) like `host_fns::register_rust_root`.
    ///
    /// # Safety
    /// The caller guarantees that `slot` is non-null, points to a valid
    /// `*mut u8` heap-pointer location, and remains valid and dereferenceable
    /// until the session ends (the `JitEffectMachine` is dropped) — the copying
    /// GC will read and rewrite `*slot` in place on every collection until then.
    /// A slot freed or moved before machine teardown is a use-after-free.
    pub unsafe fn register_persistent_root(&self, slot: *mut *mut u8) {
        // Delegates to the thread-local PERSISTENT_ROOTS registry (component D).
        // The machine is pinned to one thread for its lifetime, so the
        // thread-local and the machine share a lifetime; `free_session_heap`
        // (machine drop) clears the registry. SAFETY: forwarded to the caller's
        // contract documented above.
        crate::host_fns::register_persistent_root(slot);
    }
}

impl Drop for JitEffectMachine {
    fn drop(&mut self) {
        // Clear session-scoped thread-local roots whose slots point into the
        // session heap Vec (which drops with self after this). Harmless for
        // one-shot machines (free_session_heap does nothing if GC state is
        // already absent, and no persistent roots are registered).
        if self.session.is_some() {
            crate::host_fns::free_session_heap();
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
        if crate::host_fns::check_cancel_and_set_error() {
            vmctx.tail_callee = std::ptr::null_mut();
            vmctx.tail_arg = std::ptr::null_mut();
            ptr = crate::host_fns::error_poison_ptr();
            break;
        }

        let callee = vmctx.tail_callee;
        let arg = vmctx.tail_arg;
        vmctx.tail_callee = std::ptr::null_mut();
        vmctx.tail_arg = std::ptr::null_mut();
        crate::host_fns::reset_call_depth();
        let code_ptr =
            *(callee.add(crate::layout::CLOSURE_CODE_PTR_OFFSET as usize) as *const usize);
        let func: unsafe extern "C" fn(*mut VMContext, *mut u8, *mut u8) -> *mut u8 =
            std::mem::transmute(code_ptr);
        ptr = crate::signal_safety::with_signal_protection(|| func(vmctx, callee, arg))
            .map_err(|e| JitError::Yield(runtime_error_or_signal(e.0)))?;
    }
    Ok(ptr)
}

/// Check for a pending RuntimeError (more specific) before falling back to the
/// signal error. A runtime error like BadFunPtrTag is set by debug_app_check
/// before the JIT continues and crashes — prefer it over the raw signal number.
fn runtime_error_or_signal(sig: i32) -> crate::yield_type::YieldError {
    let fault_addr = crate::signal_safety::FAULTING_ADDR.with(|c| c.get());
    if let Some(err) = crate::host_fns::take_runtime_error() {
        if fault_addr != 0 {
            if let Some(name) = crate::debug::lookup_lambda_by_address(fault_addr) {
                crate::host_fns::push_diagnostic(format!(
                    "Faulting JIT function: {} (addr=0x{:x})",
                    name, fault_addr
                ));
            }
        }
        crate::yield_type::YieldError::from(err)
    } else {
        if fault_addr != 0 {
            if let Some(name) = crate::debug::lookup_lambda_by_address(fault_addr) {
                crate::host_fns::push_diagnostic(format!(
                    "Signal {} in JIT function: {} (addr=0x{:x})",
                    sig, name, fault_addr
                ));
            }
        }
        crate::yield_type::YieldError::Signal(sig)
    }
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
        // Set a pending runtime error via public API (kind=0 = DivisionByZero)
        crate::host_fns::runtime_error(0);

        // Signal fires after the runtime error was set
        let err = runtime_error_or_signal(libc::SIGBUS);

        // Should get DivisionByZero, not Signal(SIGBUS)
        assert_eq!(
            err,
            YieldError::Runtime(crate::host_fns::RuntimeError::DivisionByZero)
        );
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

    // ---------------------------------------------------------------------------
    // Wave 1.A seam tests
    // ---------------------------------------------------------------------------

    /// Build a Con-chain expr + DataConTable that forces >=1 GC under a small nursery.
    /// Mirrors the gc_frame_walker.rs `build_con_chain` + `make_table_with_con` helpers.
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
            });
        }
        (expr, table)
    }

    /// Test (c): persistent roots survive a RegistryGuard drop; per-run rust
    /// roots and GC state are cleared.
    #[test]
    #[serial]
    fn test_persistent_root_survives_guard_drop() {
        crate::host_fns::clear_persistent_roots();

        // Register a persistent root (null heap ptr — GC skips null slots)
        let mut persistent_slot: *mut u8 = std::ptr::null_mut();
        unsafe {
            crate::host_fns::register_persistent_root(&mut persistent_slot as *mut *mut u8);
        }
        assert_eq!(crate::host_fns::persistent_roots_count(), 1);

        // Register a per-run rust root
        let mut rust_slot: *mut u8 = std::ptr::null_mut();
        unsafe {
            crate::host_fns::register_rust_root(&mut rust_slot as *mut *mut u8);
        }
        assert_eq!(crate::host_fns::rust_roots_mark(), 1);

        // Simulate what RegistryGuard::drop does for the per-run half
        crate::host_fns::clear_run_scratch();

        // Persistent root must survive; rust roots and GC state must be gone
        assert_eq!(
            crate::host_fns::persistent_roots_count(),
            1,
            "persistent root must survive clear_run_scratch"
        );
        assert_eq!(
            crate::host_fns::rust_roots_mark(),
            0,
            "rust roots must be cleared by clear_run_scratch"
        );
        assert!(
            crate::host_fns::gc_active_range().is_none(),
            "GC state must be cleared by clear_run_scratch"
        );

        // Cleanup
        crate::host_fns::clear_persistent_roots();
    }

    /// Test (d): THE SEAM TEST — compile_session, run, verify heap retention,
    /// verify install re-points, verify persistent root survives second run.
    #[test]
    #[serial]
    fn test_session_heap_seam() {
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                crate::host_fns::clear_persistent_roots();
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
                    .as_ptr();

                // install_registries must RE-POINT at the retained buffer (not nursery.start())
                let guard = machine.install_registries();
                let (active_start, _) =
                    crate::host_fns::gc_active_range().expect("GC state installed");
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
                let mut persistent_slot: *mut u8 = std::ptr::null_mut();
                unsafe {
                    crate::host_fns::register_persistent_root(&mut persistent_slot as *mut *mut u8);
                }
                assert_eq!(crate::host_fns::persistent_roots_count(), 1);

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
                    crate::host_fns::persistent_roots_count(),
                    1,
                    "persistent root must survive run-2 teardown (clear_run_scratch)"
                );

                // Drop the machine: free_session_heap clears persistent roots
                drop(machine);
                assert_eq!(
                    crate::host_fns::persistent_roots_count(),
                    0,
                    "persistent roots must be cleared by JitEffectMachine::drop"
                );
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
