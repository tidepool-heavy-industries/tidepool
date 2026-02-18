use cranelift_codegen::ir::{self, AbiParam, types};
use cranelift_codegen::isa::TargetIsa;
use cranelift_codegen::settings::{self, Configurable};
use cranelift_codegen::Context;
use cranelift_codegen::control::ControlPlane;
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Module, Linkage, FuncId};
use std::sync::Arc;

use crate::stack_map::StackMapRegistry;

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
    pending_stack_maps: Vec<(FuncId, u32, Vec<(u32, u32, Vec<(cranelift_codegen::ir::types::Type, u32)>)>)>,
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

        let isa_builder = cranelift_native::builder().expect("host machine not supported");
        let isa = isa_builder.finish(settings::Flags::new(flag_builder.clone())).unwrap();

        let mut jit_builder = JITBuilder::with_isa(
            isa.clone(),
            cranelift_module::default_libcall_names(),
        );

        for (name, ptr) in symbols {
            jit_builder.symbol(*name, *ptr);
        }

        let module = JITModule::new(jit_builder);

        Self {
            module,
            isa,
            stack_maps: StackMapRegistry::new(),
            pending_stack_maps: Vec::new(),
        }
    }

    /// Create the standard function signature for compiled tidepool functions.
    ///
    /// Tail calling convention, vmctx: i64 first param, returns i64.
    pub fn make_func_signature(&self) -> ir::Signature {
        let mut sig = ir::Signature::new(cranelift_codegen::isa::CallConv::Tail);
        sig.params.push(AbiParam::new(types::I64)); // vmctx pointer
        sig.returns.push(AbiParam::new(types::I64)); // result pointer
        sig
    }

    /// Declare a function in the JIT module.
    pub fn declare_function(&mut self, name: &str) -> FuncId {
        let sig = self.make_func_signature();
        self.module
            .declare_function(name, Linkage::Export, &sig)
            .unwrap_or_else(|e| panic!("failed to declare function `{}`: {}", name, e))
    }

    /// Compile a function using the double-compile strategy.
    ///
    /// 1. Calls `Context::compile(isa)` to extract stack maps
    /// 2. Registers stack maps in the registry
    /// 3. Calls `module.define_function()` which recompiles for execution
    ///
    /// After calling this for all functions, call `finalize()` to make them callable.
    pub fn define_function(&mut self, func_id: FuncId, ctx: &mut Context) {
        // First compile: extract stack maps
        let mut ctrl_plane = ControlPlane::default();
        let compiled = ctx.compile(self.isa.as_ref(), &mut ctrl_plane)
            .unwrap_or_else(|e| {
                panic!("first compilation failed for function ID {:?}: {:?}", func_id, e);
            });

        let func_size = compiled.buffer.data().len() as u32;

        // Extract stack map data before define_function recompiles
        let raw_maps: Vec<(u32, u32, Vec<(cranelift_codegen::ir::types::Type, u32)>)> = compiled
            .buffer
            .user_stack_maps()
            .iter()
            .map(|(offset, span, usm)| {
                let entries: Vec<_> = usm.entries().map(|(ty, off)| (ty, off)).collect();
                (*offset, *span, entries)
            })
            .collect();

        // Second compile: define in module for execution
        self.module
            .define_function(func_id, ctx)
            .unwrap_or_else(|e| {
                panic!("define_function failed for FuncId {:?}: {:?}", func_id, e);
            });

        // Store raw maps and register after finalize.
        self.pending_stack_maps.push((func_id, func_size, raw_maps));
    }

    /// Finalize all defined functions, making them callable.
    /// Also registers stack maps now that we have function base pointers.
    pub fn finalize(&mut self) {
        self.module.finalize_definitions().unwrap();

        // Now register stack maps with actual base pointers
        let pending = std::mem::take(&mut self.pending_stack_maps);
        for (func_id, func_size, raw_maps) in pending {
            let base_ptr = self.module.get_finalized_function(func_id) as usize;
            self.stack_maps.register(base_ptr, func_size, &raw_maps);
        }
    }

    /// Get the callable function pointer after finalization.
    pub fn get_function_ptr(&self, func_id: FuncId) -> *const u8 {
        self.module.get_finalized_function(func_id)
    }
}
