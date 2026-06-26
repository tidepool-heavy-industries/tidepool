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

/// Ensures thread-local JIT registries are cleaned up even on early error return.
///
/// LIFECYCLE SEAM (Wave 1.A, review item 2/4 — frozen contract, body unchanged).
/// This drop is the per-run teardown. For the one-shot eval path that is
/// correct: the machine dies with the run. For a GHCi-style *session*, the heap
/// and roots must outlive a single run, so Wave 1.A will:
///   - move `active_buffer` (the live heap after the first GC) ownership ONTO
///     `JitEffectMachine`, so dropping `GcState` here no longer frees it;
///   - retain session-scoped `PERSISTENT_ROOTS` here (only `clear_run_scratch`
///     run-scoped state is wiped per run; `free_session_heap` runs at machine
///     drop instead — see the `host_fns` seam stubs).
///
/// So a GC fired by `run_fragment` between two fragments cannot strand a
/// persisted pointer. Until 1.A lands this calls `clear_gc_state` as before.
struct RegistryGuard;

impl Drop for RegistryGuard {
    fn drop(&mut self) {
        crate::host_fns::clear_gc_state();
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

impl JitEffectMachine {
    /// Compile a CoreExpr for JIT execution.
    pub fn compile(
        expr: &CoreExpr,
        table: &DataConTable,
        nursery_size: usize,
    ) -> Result<Self, JitError> {
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
        // One-shot path: no session bindings, so the external env is empty.
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

        Ok(Self {
            pipeline,
            nursery,
            tags,
            func_id,
            cancel_flag: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Obtain a clone-able, thread-safe handle for requesting cancellation of
    /// this machine's next (or in-flight) run. The handle remains valid for
    /// the lifetime of the machine; multiple handles may be held concurrently.
    pub fn cancel_handle(&self) -> CancelHandle {
        CancelHandle(self.cancel_flag.clone())
    }

    // LIFECYCLE SEAM (Wave 1.A): this is the per-run registry install that pairs
    // with `RegistryGuard::drop`. Wave 1.A moves `active_buffer` (live heap) +
    // persistent-root retention onto the machine here so `run_fragment` reuses a
    // persistent heap instead of a per-run nursery view. Unchanged for Wave 0.
    fn install_registries(&mut self) -> RegistryGuard {
        crate::debug::set_lambda_registry(self.pipeline.build_lambda_registry());
        crate::host_fns::set_stack_map_registry(&self.pipeline.stack_maps);
        crate::host_fns::set_gc_state(self.nursery.start() as *mut u8, self.nursery.size());
        crate::host_fns::set_cancel_flag(self.cancel_flag.clone());
        RegistryGuard
    }

    /// Run to completion, dispatching effects through the handler HList.
    pub fn run<U, H: DispatchEffect<U>>(
        &mut self,
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
        let _guard = self.install_registries();

        // SAFETY: get_function_ptr returns a finalized JIT code pointer. Transmuting to the
        // expected calling convention (vmctx -> result) is correct per our compilation contract.
        let func_ptr: unsafe extern "C" fn(*mut VMContext) -> *mut u8 =
            unsafe { std::mem::transmute(self.pipeline.get_function_ptr(self.func_id)) };
        let vmctx = self.nursery.make_vmctx(crate::host_fns::gc_trigger);

        let mut machine = CompiledEffectMachine::new(func_ptr, vmctx, tags);
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
        // Per-thread signal handler + altstack; see `run`. Idempotent.
        crate::signal_safety::install();

        // Install registries
        let _guard = self.install_registries();

        // SAFETY: get_function_ptr returns a finalized JIT code pointer. Transmuting to the
        // expected calling convention (vmctx -> result) is correct per our compilation contract.
        let func_ptr: unsafe extern "C" fn(*mut VMContext) -> *mut u8 =
            unsafe { std::mem::transmute(self.pipeline.get_function_ptr(self.func_id)) };
        let mut vmctx = self.nursery.make_vmctx(crate::host_fns::gc_trigger);

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
    // GHCi-style session re-entry (Wave 1 — components C, D, K). SCAFFOLD ONLY.
    //
    // These freeze the codegen contracts the tidepool-repl session manager
    // (Wave 2) builds on. Bodies land in Wave 1.A (lifecycle/heap ownership)
    // and 1.B (re-entry/env-seeding); see plans/ghci-swarm-orchestration.md
    // §1.A/§1.B. Do NOT implement here — these are unreachable stubs that exist
    // so later waves have a stable, conflict-free surface to code against.
    // ----------------------------------------------------------------------

    /// Compile an additional `CoreExpr` fragment into this machine's *live*
    /// `JITModule` and return its `FuncId`, without tearing down the existing
    /// code or heap. The new fragment may reference session bindings via
    /// `external_env` (Var-miss resolution to seeded heap pointers).
    ///
    /// Wave 1.B (component C1): declare + define the fragment, re-run
    /// `finalize_definitions` (verified multi-round-safe in cranelift 0.129.1 —
    /// a new `FuncId` post-finalize carves a fresh arena segment, leaving
    /// round-1 code stable), and return the id for a later [`Self::run_fragment`].
    #[allow(unused_variables)]
    pub fn add_function(
        &mut self,
        name: &str,
        expr: &CoreExpr,
        external_env: &crate::emit::ExternalEnv,
    ) -> Result<FuncId, JitError> {
        todo!("Wave 1.B: re-entrant fragment compilation (component C1)")
    }

    /// Run a previously-[`add_function`](Self::add_function)ed fragment against
    /// this machine's live, machine-owned heap, dispatching effects through the
    /// handler HList exactly as [`Self::run`] does for the one-shot entry.
    ///
    /// Wave 1.B (component C2): like `run`, but targets `func_id` instead of the
    /// machine's original `self.func_id`, reusing the persistent heap via the
    /// 1.A buffer-retention contract (must NOT re-implement run-scoped teardown).
    /// Mirrors `run`'s parameter shape.
    #[allow(unused_variables)]
    pub fn run_fragment<U, H: DispatchEffect<U>>(
        &mut self,
        func_id: FuncId,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
    ) -> Result<Value, JitError> {
        todo!("Wave 1.B: run a fragment against the live heap (component C2)")
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
}
