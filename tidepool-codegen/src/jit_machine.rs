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
#[derive(Debug)]
pub enum JitError {
    Compilation(crate::emit::EmitError),
    Pipeline(crate::pipeline::PipelineError),
    MissingConTags(&'static str),
    Effect(EffectError),
    Yield(crate::yield_type::YieldError),
    HeapBridge(crate::heap_bridge::BridgeError),
    Signal(crate::signal_safety::SignalError),
    EffectResponseTooLarge { nodes: usize, limit: usize },
}

impl std::fmt::Display for JitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JitError::Compilation(e) => write!(f, "JIT compilation error: {}", e),
            JitError::Pipeline(e) => write!(f, "pipeline error: {}", e),
            JitError::MissingConTags(name) => {
                write!(
                    f,
                    "missing freer-simple constructor '{}' in DataConTable",
                    name
                )
            }
            JitError::Effect(e) => write!(f, "effect dispatch error: {}", e),
            JitError::Yield(e) => write!(f, "yield error: {}", e),
            JitError::HeapBridge(e) => write!(f, "heap bridge error: {}", e),
            JitError::Signal(e) => write!(f, "JIT signal during heap bridge: {}", e),
            JitError::EffectResponseTooLarge { nodes, limit } => write!(
                f,
                "Effect handler response too large ({nodes} value nodes, max {limit}). \
                 Narrow your query to return fewer results."
            ),
        }
    }
}

impl std::error::Error for JitError {}

impl From<EffectError> for JitError {
    fn from(e: EffectError) -> Self {
        JitError::Effect(e)
    }
}

impl From<crate::pipeline::PipelineError> for JitError {
    fn from(e: crate::pipeline::PipelineError) -> Self {
        JitError::Pipeline(e)
    }
}

/// High-level JIT effect machine.
///
/// Compiles a `CoreExpr` (Haskell effect program) into native code via Cranelift
/// and runs it as a coroutine: the machine yields effect requests, the caller
/// dispatches them through an HList of [`EffectHandler`]s, and resumes with responses.
///
/// ```ignore
/// let (expr, table) = haskell_inline! { target = "main", include = "haskell", r#"..."# };
/// let mut vm = JitEffectMachine::compile(&expr, &table, 1 << 20)?;
/// vm.run(&table, &mut frunk::hlist![MyHandler], &())?;
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
}

/// Ensures thread-local JIT registries are cleaned up even on early error return.
struct RegistryGuard;

impl Drop for RegistryGuard {
    fn drop(&mut self) {
        crate::host_fns::clear_gc_state();
        crate::host_fns::clear_stack_map_registry();
        crate::debug::clear_lambda_registry();
    }
}

impl JitEffectMachine {
    /// Compile a CoreExpr for JIT execution.
    pub fn compile(
        expr: &CoreExpr,
        table: &DataConTable,
        nursery_size: usize,
    ) -> Result<Self, JitError> {
        let expr = crate::datacon_env::wrap_with_datacon_env(expr, table);
        let mut pipeline = CodegenPipeline::new(&crate::host_fns::host_fn_symbols())?;
        let func_id = crate::emit::expr::compile_expr(&mut pipeline, &expr, "main")
            .map_err(JitError::Compilation)?;
        pipeline.finalize()?;

        let tags = ConTags::from_table(table).map_err(|kind| kind.name());
        let nursery = Nursery::new(nursery_size);

        Ok(Self {
            pipeline,
            nursery,
            tags,
            func_id,
        })
    }

    fn install_registries(&mut self) -> RegistryGuard {
        crate::debug::set_lambda_registry(self.pipeline.build_lambda_registry());
        crate::host_fns::set_stack_map_registry(&self.pipeline.stack_maps);
        crate::host_fns::set_gc_state(self.nursery.start() as *mut u8, self.nursery.size());
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
                    let val = unsafe {
                        let vmctx_ptr = machine.vmctx_mut() as *mut VMContext;
                        crate::signal_safety::with_signal_protection(|| {
                            heap_bridge::heap_to_value_forcing(ptr, vmctx_ptr)
                        })
                    }
                    .map_err(JitError::Signal)?
                    .map_err(JitError::HeapBridge)?;
                    break Ok(val);
                }
                Yield::Request {
                    tag,
                    request,
                    continuation,
                } => {
                    // SAFETY: request is a valid heap pointer from the JIT effect dispatch.
                    let req_val = unsafe {
                        let vmctx_ptr = machine.vmctx_mut() as *mut VMContext;
                        crate::signal_safety::with_signal_protection(|| {
                            heap_bridge::heap_to_value_forcing(request, vmctx_ptr)
                        })
                    }
                    .map_err(JitError::Signal)?
                    .map_err(JitError::HeapBridge)?;
                    if std::env::var("TIDEPOOL_TRACE_EFFECTS").is_ok() {
                        eprintln!("[jit_machine] effect tag={} request={:?}", tag, req_val);
                    }
                    let cx = EffectContext::with_user(table, user);
                    let resp_val = handlers.dispatch(tag, &req_val, &cx)?;
                    const MAX_EFFECT_RESPONSE_NODES: usize = 10_000;
                    let nodes = resp_val.node_count();
                    if nodes > MAX_EFFECT_RESPONSE_NODES {
                        break Err(JitError::EffectResponseTooLarge {
                            nodes,
                            limit: MAX_EFFECT_RESPONSE_NODES,
                        });
                    }
                    // SAFETY: Converting a Value back to a heap object in the nursery.
                    // vmctx has sufficient nursery space (GC may have reclaimed).
                    let resp_ptr = unsafe {
                        crate::signal_safety::with_signal_protection(|| {
                            heap_bridge::value_to_heap(&resp_val, machine.vmctx_mut())
                        })
                    }
                    .map_err(JitError::Signal)?
                    .map_err(JitError::HeapBridge)?;
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
            Err(JitError::Yield(crate::yield_type::YieldError::from(err)))
        } else if result_ptr.is_null() {
            Err(JitError::Yield(crate::yield_type::YieldError::NullPointer))
        } else {
            // SAFETY: result_ptr is a valid heap pointer returned by the JIT.
            // vmctx_ptr is valid for forcing thunks during value conversion.
            unsafe {
                let vmctx_ptr = &mut vmctx as *mut VMContext;
                crate::signal_safety::with_signal_protection(|| {
                    heap_bridge::heap_to_value_forcing(result_ptr, vmctx_ptr)
                })
            }
            .map_err(JitError::Signal)?
            .map_err(JitError::HeapBridge)
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

/// Convert a signal error into a Yield, preferring any pending RuntimeError.
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
        assert_eq!(err, YieldError::DivisionByZero);
    }

    /// When no RuntimeError is pending, the signal number comes through.
    #[test]
    fn test_signal_passthrough_without_runtime_error() {
        // Ensure no pending error
        crate::host_fns::take_runtime_error();

        let err = runtime_error_or_signal(libc::SIGILL);
        assert_eq!(err, YieldError::Signal(libc::SIGILL));
    }
}
