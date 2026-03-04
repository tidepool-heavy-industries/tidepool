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
    MissingConTags,
    Effect(EffectError),
    Yield(crate::yield_type::YieldError),
    HeapBridge(crate::heap_bridge::BridgeError),
    EffectResponseTooLarge { nodes: usize, limit: usize },
}

impl std::fmt::Display for JitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JitError::Compilation(e) => write!(f, "JIT compilation error: {}", e),
            JitError::Pipeline(e) => write!(f, "pipeline error: {}", e),
            JitError::MissingConTags => {
                write!(f, "missing freer-simple constructors in DataConTable")
            }
            JitError::Effect(e) => write!(f, "effect dispatch error: {}", e),
            JitError::Yield(e) => write!(f, "yield error: {}", e),
            JitError::HeapBridge(e) => write!(f, "heap bridge error: {}", e),
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

/// High-level JIT effect machine — compile and run freer-simple effect stacks
/// without touching raw pointers, nurseries, or stack maps.
pub struct JitEffectMachine {
    pipeline: CodegenPipeline,
    nursery: Nursery,
    tags: Option<ConTags>,
    func_id: FuncId,
}

impl JitEffectMachine {
    /// Compile a CoreExpr for JIT execution.
    pub fn compile(
        expr: &CoreExpr,
        table: &DataConTable,
        nursery_size: usize,
    ) -> Result<Self, JitError> {
        let expr = crate::datacon_env::wrap_with_datacon_env(expr, table);
        let mut pipeline = CodegenPipeline::new(&crate::host_fns::host_fn_symbols());
        let func_id = crate::emit::expr::compile_expr(&mut pipeline, &expr, "main")
            .map_err(JitError::Compilation)?;
        pipeline.finalize()?;

        let tags = ConTags::from_table(table);
        let nursery = Nursery::new(nursery_size);

        Ok(Self {
            pipeline,
            nursery,
            tags,
            func_id,
        })
    }

    /// Run to completion, dispatching effects through the handler HList.
    pub fn run<U, H: DispatchEffect<U>>(
        &mut self,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
    ) -> Result<Value, JitError> {
        let tags = self.tags.ok_or(JitError::MissingConTags)?;

        // Install registries
        crate::debug::set_lambda_registry(self.pipeline.build_lambda_registry());
        crate::host_fns::set_stack_map_registry(&self.pipeline.stack_maps);
        crate::host_fns::set_gc_state(self.nursery.start() as *mut u8, self.nursery.size());

        let func_ptr: unsafe extern "C" fn(*mut VMContext) -> *mut u8 =
            unsafe { std::mem::transmute(self.pipeline.get_function_ptr(self.func_id)) };
        let vmctx = self.nursery.make_vmctx(crate::host_fns::gc_trigger);

        let mut machine = CompiledEffectMachine::new(func_ptr, vmctx, tags);
        crate::host_fns::reset_call_depth();
        let mut yield_result =
            match unsafe { crate::signal_safety::with_signal_protection(|| machine.step()) } {
                Ok(y) => y,
                Err(e) => signal_error_to_yield(e),
            };

        let result = loop {
            match yield_result {
                Yield::Done(ptr) => {
                    let val =
                        unsafe { heap_bridge::heap_to_value(ptr) }.map_err(JitError::HeapBridge)?;
                    break Ok(val);
                }
                Yield::Request {
                    tag,
                    request,
                    continuation,
                } => {
                    let req_val = unsafe { heap_bridge::heap_to_value(request) }
                        .map_err(JitError::HeapBridge)?;
                    let cx = EffectContext::with_user(table, user);
                    let resp_val = handlers.dispatch(tag, &req_val, &cx)?;
                    const MAX_EFFECT_RESPONSE_NODES: usize = 50_000;
                    let nodes = resp_val.node_count();
                    if nodes > MAX_EFFECT_RESPONSE_NODES {
                        break Err(JitError::EffectResponseTooLarge {
                            nodes,
                            limit: MAX_EFFECT_RESPONSE_NODES,
                        });
                    }
                    let resp_ptr =
                        unsafe { heap_bridge::value_to_heap(&resp_val, machine.vmctx_mut()) }
                            .map_err(JitError::HeapBridge)?;
                    crate::host_fns::reset_call_depth();
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

        // Cleanup registries
        crate::host_fns::clear_gc_state();
        crate::host_fns::clear_stack_map_registry();
        crate::debug::clear_lambda_registry();

        result
    }

    /// Run a pure (non-effectful) program to completion.
    ///
    /// Skips freer-simple effect dispatch entirely — calls the compiled function
    /// and converts the raw heap result directly to a Value. Use this for programs
    /// that don't use an `Eff` wrapper.
    pub fn run_pure(&mut self) -> Result<Value, JitError> {
        // Install registries
        crate::debug::set_lambda_registry(self.pipeline.build_lambda_registry());
        crate::host_fns::set_stack_map_registry(&self.pipeline.stack_maps);
        crate::host_fns::set_gc_state(self.nursery.start() as *mut u8, self.nursery.size());

        let func_ptr: unsafe extern "C" fn(*mut VMContext) -> *mut u8 =
            unsafe { std::mem::transmute(self.pipeline.get_function_ptr(self.func_id)) };
        let mut vmctx = self.nursery.make_vmctx(crate::host_fns::gc_trigger);

        crate::host_fns::reset_call_depth();
        let result_ptr: *mut u8 = match unsafe {
            crate::signal_safety::with_signal_protection(|| func_ptr(&mut vmctx))
        } {
            Ok(ptr) => ptr,
            Err(e) => {
                // Cleanup registries before returning error
                crate::host_fns::clear_gc_state();
                crate::host_fns::clear_stack_map_registry();
                crate::debug::clear_lambda_registry();
                return Err(JitError::Yield(runtime_error_or_signal(e.0)));
            }
        };

        // Check for runtime error FIRST — runtime_error now returns a poison
        // object instead of null, so we can't rely on null-check alone.
        let result = if let Some(err) = crate::host_fns::take_runtime_error() {
            Err(JitError::Yield(crate::yield_type::YieldError::from(err)))
        } else if result_ptr.is_null() {
            Err(JitError::Yield(crate::yield_type::YieldError::NullPointer))
        } else {
            unsafe { heap_bridge::heap_to_value(result_ptr) }.map_err(JitError::HeapBridge)
        };

        // Cleanup registries
        crate::host_fns::clear_gc_state();
        crate::host_fns::clear_stack_map_registry();
        crate::debug::clear_lambda_registry();

        result
    }
}

/// Check for a pending RuntimeError (more specific) before falling back to the
/// signal error. A runtime error like BadFunPtrTag is set by debug_app_check
/// before the JIT continues and crashes — prefer it over the raw signal number.
fn runtime_error_or_signal(sig: i32) -> crate::yield_type::YieldError {
    if let Some(err) = crate::host_fns::take_runtime_error() {
        crate::yield_type::YieldError::from(err)
    } else {
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
