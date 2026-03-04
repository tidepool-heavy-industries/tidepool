use cranelift_codegen::control::ControlPlane;
use cranelift_codegen::ir::{self, types, AbiParam};
use cranelift_codegen::isa::TargetIsa;
use cranelift_codegen::settings::{self, Configurable};
use cranelift_codegen::Context;
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module};
use std::sync::Arc;

use crate::debug::LambdaRegistry;
use crate::stack_map::{RawStackMap, StackMapRegistry};

/// Errors from the Cranelift compilation pipeline.
#[derive(Debug)]
pub enum PipelineError {
    /// Function declaration failed.
    Declaration(String),
    /// First-pass compilation failed (stack map extraction).
    Compilation(String),
    /// Module define_function failed.
    Definition(String),
    /// Module finalize_definitions failed.
    Finalization(String),
}

impl std::fmt::Display for PipelineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PipelineError::Declaration(e) => write!(f, "function declaration failed: {}", e),
            PipelineError::Compilation(e) => write!(f, "compilation failed: {}", e),
            PipelineError::Definition(e) => write!(f, "define_function failed: {}", e),
            PipelineError::Finalization(e) => write!(f, "finalize_definitions failed: {}", e),
        }
    }
}

impl std::error::Error for PipelineError {}

/// Cranelift JIT compilation pipeline.
///
/// Implements the double-compile strategy:
/// 1. `Context::compile(isa)` to extract stack maps from CompiledCode
/// 2. `module.define_function()` for executable code (recompiles from IR)
pub struct CodegenPipeline {
    /// The JIT module that manages executable memory.
    ///
    /// This field is public as an **escape hatch** for advanced use cases and tests
    /// that need direct access to Cranelift's `JITModule`. Most users should prefer
    /// the safe wrapper methods on `CodegenPipeline` (e.g., `declare_function`)
    /// instead of calling into `module` directly.
    pub module: JITModule,
    /// Target ISA (needed for Context::compile).
    pub isa: Arc<dyn TargetIsa>,
    /// Stack map registry populated during compilation.
    pub stack_maps: StackMapRegistry,
    /// Pending stack maps waiting for finalization to get base pointers.
    /// Stores (func_id, func_size, raw_maps).
    pending_stack_maps: Vec<(FuncId, u32, Vec<RawStackMap>)>,
    /// Lambda name registry: (func_id, name). Populated during define_function.
    lambda_names: Vec<(FuncId, String)>,
}

impl CodegenPipeline {
    /// Create a new CodegenPipeline with default x86-64 settings.
    ///
    /// `symbols` is a list of (name, pointer) pairs for host functions
    /// that JIT code can call (e.g., gc_trigger, heap_alloc).
    pub fn new(symbols: &[(&str, *const u8)]) -> Self {
        let mut flag_builder = settings::builder();
        // REQUIRED: enables RBP frame chain for GC stack walking
        flag_builder.set("preserve_frame_pointers", "true").unwrap();
        flag_builder.set("opt_level", "speed").unwrap();
        // PIC mode: external symbols go through GOT entries so x86 PC-relative
        // relocations don't overflow when JIT code is >2GB from host functions.
        // Matches what JITBuilder::new() sets internally.
        flag_builder.set("is_pic", "true").unwrap();
        flag_builder.set("use_colocated_libcalls", "false").unwrap();

        let isa_builder = cranelift_native::builder().expect("host machine not supported");
        let isa = isa_builder
            .finish(settings::Flags::new(flag_builder.clone()))
            .unwrap();

        let mut jit_builder =
            JITBuilder::with_isa(isa.clone(), cranelift_module::default_libcall_names());

        for (name, ptr) in symbols {
            jit_builder.symbol(*name, *ptr);
        }

        let module = JITModule::new(jit_builder);

        Self {
            module,
            isa,
            stack_maps: StackMapRegistry::new(),
            pending_stack_maps: Vec::new(),
            lambda_names: Vec::new(),
        }
    }

    /// Create the standard function signature for compiled tidepool functions.
    ///
    /// Uses the target ISA's default C ABI calling convention, with vmctx: i64
    /// as the first parameter and an i64 return value.
    pub fn make_func_signature(&self) -> ir::Signature {
        let mut sig = ir::Signature::new(self.isa.default_call_conv());
        sig.params.push(AbiParam::new(types::I64)); // vmctx pointer
        sig.returns.push(AbiParam::new(types::I64)); // result pointer
        sig
    }

    /// Declare a function in the JIT module.
    pub fn declare_function(&mut self, name: &str) -> Result<FuncId, PipelineError> {
        let sig = self.make_func_signature();
        self.module
            .declare_function(name, Linkage::Export, &sig)
            .map_err(|e| PipelineError::Declaration(format!("failed to declare `{}`: {}", name, e)))
    }

    /// Compile a function using the double-compile strategy.
    ///
    /// 1. Calls `Context::compile(isa)` to extract stack maps
    /// 2. Registers stack maps in the registry
    /// 3. Calls `module.define_function()` which recompiles for execution
    ///
    /// After calling this for all functions, call `finalize()` to make them callable.
    pub fn define_function(
        &mut self,
        func_id: FuncId,
        ctx: &mut Context,
    ) -> Result<(), PipelineError> {
        // First compile: extract stack maps
        let mut ctrl_plane = ControlPlane::default();
        let compiled = ctx
            .compile(self.isa.as_ref(), &mut ctrl_plane)
            .map_err(|e| PipelineError::Compilation(format!("{:?}", e)))?;

        let func_size = compiled.buffer.data().len() as u32;

        // Extract stack map data before define_function recompiles
        let raw_maps: Vec<RawStackMap> = compiled
            .buffer
            .user_stack_maps()
            .iter()
            .map(|(offset, span, usm)| {
                let entries: Vec<_> = usm.entries().collect();
                (*offset, *span, entries)
            })
            .collect();

        // Second compile: define in module for execution
        self.module
            .define_function(func_id, ctx)
            .map_err(|e| PipelineError::Definition(format!("{:?}", e)))?;

        // Store raw maps and register after finalize.
        self.pending_stack_maps.push((func_id, func_size, raw_maps));
        Ok(())
    }

    /// Finalize all defined functions, making them callable.
    /// Also registers stack maps now that we have function base pointers.
    pub fn finalize(&mut self) -> Result<(), PipelineError> {
        self.module
            .finalize_definitions()
            .map_err(|e| PipelineError::Finalization(format!("{}", e)))?;

        // Now register stack maps with actual base pointers
        let pending = std::mem::take(&mut self.pending_stack_maps);
        for (func_id, func_size, raw_maps) in pending {
            let base_ptr = self.module.get_finalized_function(func_id) as usize;
            self.stack_maps.register(base_ptr, func_size, &raw_maps);
        }
        Ok(())
    }

    /// Get the callable function pointer after finalization.
    pub fn get_function_ptr(&self, func_id: FuncId) -> *const u8 {
        self.module.get_finalized_function(func_id)
    }

    /// Register a lambda name for a function ID (call before finalize).
    pub fn register_lambda(&mut self, func_id: FuncId, name: String) {
        self.lambda_names.push((func_id, name));
    }

    /// Build a LambdaRegistry from all registered lambdas.
    /// Must be called after `finalize()` so code pointers are available.
    pub fn build_lambda_registry(&self) -> LambdaRegistry {
        let mut registry = LambdaRegistry::new();
        for (func_id, name) in &self.lambda_names {
            let ptr = self.module.get_finalized_function(*func_id) as usize;
            registry.register(ptr, name.clone());
        }
        registry
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cranelift_codegen::ir::InstBuilder;
    use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};

    #[test]
    fn test_empty_pipeline() {
        let mut pipeline = CodegenPipeline::new(&[]);
        pipeline.finalize().unwrap();
    }

    #[test]
    fn test_declare_define_finalize() {
        let mut pipeline = CodegenPipeline::new(&[]);
        let func_id = pipeline.declare_function("test_fn").unwrap();

        let mut ctx = pipeline.module.make_context();
        ctx.func.signature = pipeline.make_func_signature();

        let mut builder_context = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_context);

        let block = builder.create_block();
        builder.append_block_params_for_function_params(block);
        builder.switch_to_block(block);
        builder.seal_block(block);

        let val = builder.ins().iconst(types::I64, 42);
        builder.ins().return_(&[val]);
        builder.finalize();

        pipeline.define_function(func_id, &mut ctx).unwrap();
        pipeline.finalize().unwrap();

        let ptr = pipeline.get_function_ptr(func_id);
        assert!(!ptr.is_null());

        let func: unsafe extern "C" fn(usize) -> i64 = unsafe { std::mem::transmute(ptr) };
        let res = unsafe { func(0) };
        assert_eq!(res, 42);
    }

    #[test]
    fn test_duplicate_declarations() {
        let mut pipeline = CodegenPipeline::new(&[]);
        let id1 = pipeline.declare_function("f1").unwrap();
        let id2 = pipeline.declare_function("f2").unwrap();
        assert_ne!(id1, id2);

        let id3 = pipeline.declare_function("f1").unwrap();
        assert_eq!(id1, id3);
    }

    #[test]
    fn test_get_function_ptr_after_finalize() {
        let mut pipeline = CodegenPipeline::new(&[]);
        let func_id = pipeline.declare_function("f1").unwrap();

        let mut ctx = pipeline.module.make_context();
        ctx.func.signature = pipeline.make_func_signature();
        let mut builder_context = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_context);
        let block = builder.create_block();
        builder.append_block_params_for_function_params(block);
        builder.switch_to_block(block);
        builder.seal_block(block);
        let val = builder.ins().iconst(types::I64, 0);
        builder.ins().return_(&[val]);
        builder.finalize();

        pipeline.define_function(func_id, &mut ctx).unwrap();
        pipeline.finalize().unwrap();

        let ptr = pipeline.get_function_ptr(func_id);
        assert!(!ptr.is_null());
    }

    #[test]
    fn test_build_lambda_registry() {
        let mut pipeline = CodegenPipeline::new(&[]);
        let func_id = pipeline.declare_function("f1").unwrap();

        let mut ctx = pipeline.module.make_context();
        ctx.func.signature = pipeline.make_func_signature();
        let mut builder_context = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_context);
        let block = builder.create_block();
        builder.append_block_params_for_function_params(block);
        builder.switch_to_block(block);
        builder.seal_block(block);
        let val = builder.ins().iconst(types::I64, 0);
        builder.ins().return_(&[val]);
        builder.finalize();

        pipeline.define_function(func_id, &mut ctx).unwrap();
        pipeline.register_lambda(func_id, "my_lambda".to_string());
        pipeline.finalize().unwrap();

        let registry = pipeline.build_lambda_registry();
        let ptr = pipeline.get_function_ptr(func_id);
        assert_eq!(registry.lookup(ptr as usize), Some("my_lambda"));
    }

    #[test]
    fn test_host_fn_symbols_integration() {
        extern "C" fn my_host_fn() -> i64 {
            123
        }
        let symbols = [("my_host_fn", my_host_fn as *const u8)];
        let mut pipeline = CodegenPipeline::new(&symbols);

        let func_id = pipeline.declare_function("call_host").unwrap();
        let mut ctx = pipeline.module.make_context();
        ctx.func.signature = pipeline.make_func_signature();

        let mut builder_context = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_context);

        let block = builder.create_block();
        builder.append_block_params_for_function_params(block);
        builder.switch_to_block(block);
        builder.seal_block(block);

        let mut sig = ir::Signature::new(pipeline.isa.default_call_conv());
        sig.returns.push(AbiParam::new(types::I64));
        let callee = pipeline
            .module
            .declare_function("my_host_fn", Linkage::Import, &sig)
            .unwrap();
        let local_callee = pipeline
            .module
            .declare_func_in_func(callee, &mut builder.func);

        let call = builder.ins().call(local_callee, &[]);
        let res = builder.inst_results(call)[0];
        builder.ins().return_(&[res]);
        builder.finalize();

        pipeline.define_function(func_id, &mut ctx).unwrap();
        pipeline.finalize().unwrap();

        let ptr = pipeline.get_function_ptr(func_id);
        let func: unsafe extern "C" fn(usize) -> i64 = unsafe { std::mem::transmute(ptr) };
        assert_eq!(unsafe { func(0) }, 123);
    }
}
