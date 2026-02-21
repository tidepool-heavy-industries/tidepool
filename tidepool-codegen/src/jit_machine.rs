use tidepool_effect::{DispatchEffect, EffectContext, EffectError};
use tidepool_eval::value::Value;
use tidepool_repr::{CoreExpr, DataConTable};
use cranelift_module::FuncId;

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
}

impl std::fmt::Display for JitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JitError::Compilation(e) => write!(f, "JIT compilation error: {}", e),
            JitError::Pipeline(e) => write!(f, "pipeline error: {}", e),
            JitError::MissingConTags => write!(f, "missing freer-simple constructors in DataConTable"),
            JitError::Effect(e) => write!(f, "effect dispatch error: {}", e),
            JitError::Yield(e) => write!(f, "yield error: {}", e),
            JitError::HeapBridge(e) => write!(f, "heap bridge error: {}", e),
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

        let func_ptr: unsafe extern "C" fn(*mut VMContext) -> *mut u8 =
            unsafe { std::mem::transmute(self.pipeline.get_function_ptr(self.func_id)) };
        let vmctx = self.nursery.make_vmctx(crate::host_fns::gc_trigger);

        let mut machine = CompiledEffectMachine::new(func_ptr, vmctx, tags);
        let mut yield_result = machine.step();

        let result = loop {
            match yield_result {
                Yield::Done(ptr) => {
                    let val = unsafe { heap_bridge::heap_to_value(ptr) }
                        .map_err(JitError::HeapBridge)?;
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
                    let resp_ptr =
                        unsafe { heap_bridge::value_to_heap(&resp_val, machine.vmctx_mut()) }
                            .map_err(JitError::HeapBridge)?;
                    yield_result = unsafe { machine.resume(continuation, resp_ptr) };
                }
                Yield::Error(e) => break Err(JitError::Yield(e)),
            }
        };

        // Cleanup registries
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

        let func_ptr: unsafe extern "C" fn(*mut VMContext) -> *mut u8 =
            unsafe { std::mem::transmute(self.pipeline.get_function_ptr(self.func_id)) };
        let mut vmctx = self.nursery.make_vmctx(crate::host_fns::gc_trigger);

        let result_ptr: *mut u8 = unsafe { func_ptr(&mut vmctx) };

        let result = if result_ptr.is_null() {
            if let Some(err) = crate::host_fns::take_runtime_error() {
                Err(JitError::Yield(match err {
                    crate::host_fns::RuntimeError::DivisionByZero => {
                        crate::yield_type::YieldError::DivisionByZero
                    }
                    crate::host_fns::RuntimeError::Overflow => {
                        crate::yield_type::YieldError::Overflow
                    }
                }))
            } else {
                Err(JitError::Yield(crate::yield_type::YieldError::NullPointer))
            }
        } else {
            unsafe { heap_bridge::heap_to_value(result_ptr) }.map_err(JitError::HeapBridge)
        };

        // Cleanup registries
        crate::host_fns::clear_stack_map_registry();
        crate::debug::clear_lambda_registry();

        result
    }
}
