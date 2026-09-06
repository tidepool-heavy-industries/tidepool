use cranelift_codegen::ir::{self, types, AbiParam};
use cranelift_codegen::isa::TargetIsa;
use cranelift_codegen::settings::{self, Configurable};
use cranelift_codegen::Context;
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module};
use std::collections::HashSet;
use std::mem::ManuallyDrop;
use std::ops::{Deref, DerefMut};
use std::rc::Rc;
use std::sync::Arc;

use crate::debug::LambdaRegistry;
use crate::stack_map::{RawStackMap, RawStackMapEntry, StackMapRegistry};

/// Owns executable allocations for exactly the module's lifetime. Raw code and
/// data pointers obtained through this module must not be used after its drop.
/// Cranelift otherwise deliberately leaks finalized allocations on drop.
pub struct OwnedJitModule(ManuallyDrop<JITModule>);

impl OwnedJitModule {
    fn new(module: JITModule) -> Self {
        Self(ManuallyDrop::new(module))
    }
}

impl Deref for OwnedJitModule {
    type Target = JITModule;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for OwnedJitModule {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for OwnedJitModule {
    fn drop(&mut self) {
        // SAFETY: drop has exclusive ownership, so no safe module borrow or
        // synchronous JIT invocation remains. Escaped raw pointers carry this
        // owner's lifetime contract. Taking the module exactly once prevents
        // its default leaking drop from bypassing allocation cleanup.
        unsafe { ManuallyDrop::take(&mut self.0).free_memory() };
    }
}

/// Errors from the Cranelift compilation pipeline.
#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error("pipeline contains an incomplete compilation and must be retired")]
    IncompleteCompilation,
    /// Pipeline initialization failed (ISA detection, memory reservation).
    #[error("pipeline init failed: {0}")]
    Init(String),
    /// Function declaration failed.
    #[error("function declaration failed: {0}")]
    Declaration(String),
    /// First-pass compilation failed (stack map extraction).
    #[error("compilation failed: {0}")]
    Compilation(String),
    /// Module define_function failed.
    #[error("define_function failed: {0}")]
    Definition(String),
    /// Module finalize_definitions failed.
    #[error("finalize_definitions failed: {0}")]
    Finalization(String),
}

const COMPILER_STACK_RESERVE: usize = 1024 * 1024;
const COMPILER_STACK_SEGMENT: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum CompilationState {
    Ready,
    Failed,
}

/// Cranelift JIT compilation pipeline.
///
/// Single-compile strategy: `module.define_function()` compiles and links,
/// then stack maps are extracted from `ctx.compiled_code()`.
pub struct CodegenPipeline {
    compilation_state: CompilationState,
    /// The JIT module that manages executable memory.
    ///
    /// This field is public as an **escape hatch** for advanced use cases and tests
    /// that need direct access to Cranelift's `JITModule`. Most users should prefer
    /// the safe wrapper methods on `CodegenPipeline` (e.g., `declare_function`)
    /// instead of calling into `module` directly.
    pub module: OwnedJitModule,
    /// Target ISA (needed for Context::compile).
    pub isa: Arc<dyn TargetIsa>,
    /// Stack map registry populated during compilation.
    pub stack_maps: StackMapRegistry,
    /// Pending stack maps waiting for finalization to get base pointers.
    /// Stores (func_id, func_size, raw_maps).
    pending_stack_maps: Vec<(FuncId, u32, Vec<RawStackMap>)>,
    /// Lambda name registry: (func_id, name). Populated during define_function.
    /// Never truncated — `lambda_registry`/`lambda_registry_built_upto` below
    /// track how much of this has already been folded into the accumulated
    /// registry, so a fresh `build_lambda_registry` call only walks the tail.
    lambda_names: Vec<(FuncId, String)>,
    /// Accumulated code-ptr -> name registry, shared via `Rc` with whatever
    /// thread-local slot last installed it (`debug::set_lambda_registry`).
    /// `build_lambda_registry` extends this in place: once the previous
    /// run's thread-local handle is dropped (refcount back to 1), extending
    /// is `Rc::make_mut` + insert of only the NEW entries, not a rebuild of
    /// the whole session's lambda history.
    lambda_registry: Rc<LambdaRegistry>,
    /// How many of `lambda_names`'s entries are already folded into
    /// `lambda_registry`.
    lambda_registry_built_upto: usize,
    /// Only finalized names may be resolved, including during failed-job cleanup.
    lambda_names_finalized: usize,
    /// Boxed-literal wrapper constructor ids (I#/W#/C#/F#/D#) for this compile,
    /// set by the JIT entry point from the DataConTable. Transported here so
    /// `compile_expr` can stamp it onto every `EmitSession` without threading
    /// the table through its signature. Defaults to empty (no wrapper
    /// tolerance), which preserves behavior for direct test callers.
    pub lit_wrappers: crate::emit::LitWrapperIds,
    /// Session-lifetime count of Cranelift functions successfully compiled
    /// (fragment entries, lambda bodies, thunk bodies — every
    /// [`Self::define_function`] call that returned `Ok`). Never reset, so
    /// the delta across one `add_function` call is exactly how much Cranelift
    /// work that turn caused: a delta that stays large and roughly constant
    /// across turns on the same session means constructor closures are being
    /// re-declared and re-compiled every turn rather than reused.
    functions_defined: u64,
    /// Session-lifetime count of Cranelift IR blocks across every
    /// successfully compiled function (summed at each
    /// [`Self::define_function`] call, from `ctx.func.layout.blocks().count()`
    /// of the function just compiled). Never reset, so the delta across one
    /// `add_function` call is the total block count Cranelift emitted for
    /// that turn's functions — read it the same snapshot-before/diff-after
    /// way as [`Self::functions_defined`].
    blocks_emitted: u64,
    /// String-intern arena for diagnostic strings (e.g. enclosing-function
    /// names) that compiled code holds a raw pointer to.
    ///
    /// INVARIANT: this arena outlives all code compiled by this pipeline —
    /// a pointer handed out by [`Self::intern_name`] is valid exactly as
    /// long as `self` is alive. A `Box<str>`'s heap-allocated bytes don't
    /// move when the `HashSet` rehashes (only the box handle does), so the
    /// pointer stays stable across further `intern_name` calls. Deduped by
    /// name, so N case sites in one function share one allocation, and
    /// recompiling the same function doesn't grow the set.
    name_arena: HashSet<Box<str>>,
}

impl CodegenPipeline {
    /// Create a new CodegenPipeline with default x86-64 settings.
    ///
    /// `symbols` is a list of (name, pointer) pairs for host functions
    /// that JIT code can call (e.g., gc_trigger).
    pub fn new(symbols: &[(&str, *const u8)]) -> Result<Self, PipelineError> {
        let mut flag_builder = settings::builder();
        // REQUIRED: enables RBP frame chain for GC stack walking
        flag_builder
            .set("preserve_frame_pointers", "true")
            .map_err(|e| PipelineError::Init(format!("set preserve_frame_pointers: {e}")))?;
        flag_builder
            .set("opt_level", "speed")
            .map_err(|e| PipelineError::Init(format!("set opt_level: {e}")))?;
        // cranelift-jit requires non-PIC code. References between independently
        // allocated functions/data must also be non-colocated (see define_function).
        flag_builder
            .set("is_pic", "false")
            .map_err(|e| PipelineError::Init(format!("set is_pic: {e}")))?;
        flag_builder
            .set("use_colocated_libcalls", "false")
            .map_err(|e| PipelineError::Init(format!("set use_colocated_libcalls: {e}")))?;

        let isa_builder = cranelift_native::builder()
            .map_err(|e| PipelineError::Init(format!("host ISA: {e}")))?;
        let isa = isa_builder
            .finish(settings::Flags::new(flag_builder.clone()))
            .map_err(|e| PipelineError::Init(format!("ISA finish: {e}")))?;

        let mut jit_builder =
            JITBuilder::with_isa(isa.clone(), cranelift_module::default_libcall_names());

        for (name, ptr) in symbols {
            jit_builder.symbol(*name, *ptr);
        }

        // The default SystemMemoryProvider grows on demand without a fixed
        // contiguous reservation. Finalized allocations keep stable addresses.
        let module = OwnedJitModule::new(JITModule::new(jit_builder));

        Ok(Self {
            compilation_state: CompilationState::Ready,
            module,
            isa,
            stack_maps: StackMapRegistry::new(),
            pending_stack_maps: Vec::new(),
            lambda_names: Vec::new(),
            lambda_registry: Rc::new(LambdaRegistry::new()),
            lambda_registry_built_upto: 0,
            lambda_names_finalized: 0,
            lit_wrappers: crate::emit::LitWrapperIds::default(),
            functions_defined: 0,
            blocks_emitted: 0,
            name_arena: HashSet::new(),
        })
    }

    /// An incomplete module cannot be finalized or accept another fragment.
    pub fn compilation_failed(&self) -> bool {
        self.compilation_state == CompilationState::Failed
    }

    pub(crate) fn ensure_usable(&self) -> Result<(), PipelineError> {
        if self.compilation_failed() {
            Err(PipelineError::IncompleteCompilation)
        } else {
            Ok(())
        }
    }

    pub(crate) fn invalidate(&mut self) {
        self.compilation_state = CompilationState::Failed;
    }

    /// Intern `name` in the pipeline-owned arena, returning a raw pointer +
    /// length valid for the pipeline's lifetime. Dedupes by name, so
    /// repeated interning of the same name (e.g. multiple case sites in one
    /// function, or recompiling the same function) returns the same
    /// allocation instead of growing the arena.
    pub fn intern_name(&mut self, name: &str) -> (*const u8, usize) {
        if let Some(existing) = self.name_arena.get(name) {
            return (existing.as_ptr(), existing.len());
        }
        let boxed: Box<str> = name.into();
        let ptr = boxed.as_ptr();
        let len = boxed.len();
        self.name_arena.insert(boxed);
        (ptr, len)
    }

    /// Number of distinct names currently interned. Test/diagnostic hook.
    pub fn interned_name_count(&self) -> usize {
        self.name_arena.len()
    }

    /// Session-lifetime count of Cranelift functions successfully compiled.
    /// See the `functions_defined` field doc for how to read a delta.
    pub fn functions_defined(&self) -> u64 {
        self.functions_defined
    }

    /// Session-lifetime count of Cranelift IR blocks emitted. See the
    /// `blocks_emitted` field doc for how to read a delta.
    pub fn blocks_emitted(&self) -> u64 {
        self.blocks_emitted
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
        self.ensure_usable()?;
        let sig = self.make_func_signature();
        self.module
            .declare_function(name, Linkage::Export, &sig)
            .map_err(|e| PipelineError::Declaration(format!("failed to declare `{}`: {}", name, e)))
    }

    /// Compile and define a function in the JIT module.
    ///
    /// `define_function` internally calls `ctx.compile()`, then stack maps
    /// are extracted from `ctx.compiled_code()` — single compile per function.
    ///
    /// After calling this for all functions, call `finalize()` to make them callable.
    pub fn define_function(
        &mut self,
        func_id: FuncId,
        ctx: &mut Context,
    ) -> Result<(), PipelineError> {
        self.ensure_usable()?;
        // Cranelift has its own substantial native frames. Emission guards
        // cannot protect compilation after returning to their caller's stack.
        self.invalidate();
        let result = stacker::maybe_grow(COMPILER_STACK_RESERVE, COMPILER_STACK_SEGMENT, || {
            self.define_function_inner(func_id, ctx)
        });
        if result.is_ok() {
            self.compilation_state = CompilationState::Ready;
        }
        result
    }

    fn define_function_inner(
        &mut self,
        func_id: FuncId,
        ctx: &mut Context,
    ) -> Result<(), PipelineError> {
        // Module linkage describes symbol visibility, not physical proximity.
        // Cranelift marks local/exported references colocated by default; that
        // permits short-range relocations which an on-demand allocator cannot
        // guarantee. Enforce the allocation contract once, for every emitter.
        for function in ctx.func.dfg.ext_funcs.values_mut() {
            function.colocated = false;
        }
        for value in ctx.func.global_values.values_mut() {
            if let ir::GlobalValueData::Symbol { colocated, .. } = value {
                *colocated = false;
            }
        }
        // Single compile: define_function internally calls ctx.compile()
        self.module
            .define_function(func_id, ctx)
            .map_err(|e| PipelineError::Definition(format!("{:?}", e)))?;

        // Extract stack maps from the same compilation
        let compiled = ctx.compiled_code().ok_or_else(|| {
            PipelineError::Compilation("compiled_code missing after define_function".into())
        })?;
        let func_size = compiled.code_buffer().len() as u32;
        let raw_maps: Vec<RawStackMap> = compiled
            .buffer
            .user_stack_maps()
            .iter()
            .map(|(offset, span, usm)| {
                let entries: Vec<_> = usm
                    .entries()
                    .map(|(ty, offset)| RawStackMapEntry { ty, offset })
                    .collect();
                RawStackMap {
                    code_offset: *offset,
                    frame_size: *span,
                    entries,
                }
            })
            .collect();

        self.pending_stack_maps.push((func_id, func_size, raw_maps));
        self.functions_defined += 1;
        self.blocks_emitted += ctx.func.layout.blocks().count() as u64;
        Ok(())
    }

    /// Finalize all defined functions, making them callable.
    /// Also registers stack maps now that we have function base pointers.
    pub fn finalize(&mut self) -> Result<(), PipelineError> {
        self.ensure_usable()?;
        self.invalidate();
        self.module
            .finalize_definitions()
            .map_err(|e| PipelineError::Finalization(e.to_string()))?;

        // Now register stack maps with actual base pointers
        let pending = std::mem::take(&mut self.pending_stack_maps);
        for (func_id, func_size, raw_maps) in pending {
            let base_ptr = self.module.get_finalized_function(func_id) as usize;
            self.stack_maps.register(base_ptr, func_size, &raw_maps);
        }
        self.lambda_names_finalized = self.lambda_names.len();
        self.compilation_state = CompilationState::Ready;
        Ok(())
    }

    /// Get the callable function pointer after finalization. The pointer remains
    /// valid only while this pipeline's module owner is alive.
    pub fn get_function_ptr(&self, func_id: FuncId) -> *const u8 {
        self.module.get_finalized_function(func_id)
    }

    /// Register a lambda name for a function ID (call before finalize).
    pub fn register_lambda(&mut self, func_id: FuncId, name: String) {
        self.lambda_names.push((func_id, name));
    }

    /// Return the accumulated `LambdaRegistry`, incrementally extended with
    /// any lambdas registered since the last call.
    ///
    /// Must be called after `finalize()` so code pointers are available for
    /// the newly-registered entries. Once resolved, a JIT function's code
    /// pointer never moves (finalized allocations stay at stable addresses),
    /// so entries folded in on an earlier call
    /// stay valid forever and never need re-resolving.
    ///
    /// `Rc::make_mut` extends in place (O(new entries)) when this is the only
    /// outstanding handle — true whenever the previous run's thread-local
    /// install has already been cleared (`debug::clear_lambda_registry`,
    /// called from `RegistryGuard::drop` before the next run starts). It
    /// falls back to a clone-then-extend only if a handle is still
    /// outstanding (e.g. a nested child run compiling new lambdas while the
    /// parent's registry handle is still installed) — correctness-preserving,
    /// just not O(1)/O(new) in that rarer reentrant case.
    pub fn build_lambda_registry(&mut self) -> Rc<LambdaRegistry> {
        if self.lambda_registry_built_upto < self.lambda_names_finalized {
            let registry = Rc::make_mut(&mut self.lambda_registry);
            for (func_id, name) in
                &self.lambda_names[self.lambda_registry_built_upto..self.lambda_names_finalized]
            {
                let ptr = self.module.get_finalized_function(*func_id) as usize;
                registry.register(ptr, name.clone());
            }
            self.lambda_registry_built_upto = self.lambda_names_finalized;
        }
        Rc::clone(&self.lambda_registry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cranelift_codegen::ir::InstBuilder;
    use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
    use std::collections::HashMap;

    #[cfg(target_os = "linux")]
    pub(crate) fn address_is_executable(address: usize) -> bool {
        std::fs::read_to_string("/proc/self/maps")
            .unwrap()
            .lines()
            .any(|line| {
                let mut fields = line.split_whitespace();
                let (start, end) = fields.next().unwrap().split_once('-').unwrap();
                let start = usize::from_str_radix(start, 16).unwrap();
                let end = usize::from_str_radix(end, 16).unwrap();
                (start..end).contains(&address) && fields.next().unwrap().contains('x')
            })
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn module_drop_releases_code_after_success_and_definition_failure() {
        enum Failure {
            None,
            Definition,
            ClosureJob,
        }
        for fail in [Failure::None, Failure::Definition, Failure::ClosureJob] {
            let address = {
                let mut pipeline = CodegenPipeline::new(&[]).unwrap();
                let function = define_trivial_lambda(&mut pipeline, "owned_code", 42);
                pipeline.finalize().unwrap();
                let address = pipeline.get_function_ptr(function) as usize;
                assert!(address_is_executable(address));
                match fail {
                    Failure::None => {}
                    Failure::Definition => {
                        let mut ctx = pipeline.module.make_context();
                        assert!(pipeline.define_function(function, &mut ctx).is_err());
                    }
                    Failure::ClosureJob => {
                        use tidepool_repr::{CoreFrame, PrimOpKind, TreeBuilder, VarId};
                        let mut tree = TreeBuilder::new();
                        let invalid = tree.push(CoreFrame::PrimOp {
                            op: PrimOpKind::SeqOp,
                            args: vec![],
                        });
                        tree.push(CoreFrame::Lam {
                            binder: VarId(1),
                            body: invalid,
                        });
                        assert!(crate::emit::expr::compile_expr(
                            &mut pipeline,
                            &tree.build(),
                            "failed_job",
                            &crate::emit::ExternalEnv::new()
                        )
                        .is_err());
                        assert!(pipeline.compilation_failed());
                    }
                }
                address
            };
            assert!(!address_is_executable(address));
        }
    }

    #[test]
    fn incremental_definitions_grow_past_256_mib() {
        let mut pipeline = CodegenPipeline::new(&[]).unwrap();
        let leaf = define_trivial_lambda(&mut pipeline, "retained_leaf", 42);
        pipeline.finalize().unwrap();
        let leaf_ptr = pipeline.get_function_ptr(leaf);
        let mut first_data = None;
        for _ in 0..9 {
            let data = pipeline
                .module
                .declare_anonymous_data(false, false)
                .unwrap();
            let mut description = cranelift_module::DataDescription::new();
            description.define_zeroinit(32 * 1024 * 1024);
            pipeline.module.define_data(data, &description).unwrap();
            pipeline.finalize().unwrap();
            let (address, size) = pipeline.module.get_finalized_data(data);
            assert_eq!(size, 32 * 1024 * 1024);
            let retained = *first_data.get_or_insert(address);
            // SAFETY: these finalized allocations remain live, and both reads
            // are within their declared data sizes.
            unsafe {
                assert_eq!(address.add(size - 1).read(), 0);
                assert_eq!(retained.read(), 0);
                let call: unsafe extern "C" fn(usize) -> i64 = std::mem::transmute(leaf_ptr);
                assert_eq!(call(0), 42);
            }
        }
        drop(pipeline);
    }

    #[test]
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    fn distant_code_and_data_remain_callable_across_rounds() {
        // Disjoint 4 GiB reservations put their small allocations beyond the
        // signed 32-bit relocation range without touching the unused pages.
        let make_pipeline = |symbols: &[(&str, *const u8)]| {
            let mut pipeline = CodegenPipeline::new(&[]).unwrap();
            let mut jit = JITBuilder::with_isa(
                pipeline.isa.clone(),
                cranelift_module::default_libcall_names(),
            );
            for (name, address) in symbols {
                jit.symbol(*name, *address);
            }
            jit.memory_provider(Box::new(
                cranelift_jit::ArenaMemoryProvider::new_with_size(4 << 30).unwrap(),
            ));
            pipeline.module = OwnedJitModule::new(JITModule::new(jit));
            pipeline
        };
        let mut definitions = make_pipeline(&[]);
        let leaf = define_trivial_lambda(&mut definitions, "distant_leaf", 42);
        definitions.finalize().unwrap();
        let leaf_ptr = definitions.get_function_ptr(leaf);
        let data = definitions
            .module
            .declare_anonymous_data(false, false)
            .unwrap();
        let mut description = cranelift_module::DataDescription::new();
        description.define(19_i64.to_ne_bytes().to_vec().into_boxed_slice());
        definitions.module.define_data(data, &description).unwrap();
        definitions.finalize().unwrap();
        let data_ptr = definitions.module.get_finalized_data(data).0;

        let mut pipeline = make_pipeline(&[("distant_leaf", leaf_ptr), ("distant_data", data_ptr)]);
        // Export linkage asks Cranelift for colocated references by default,
        // just like emitted local lambdas and anonymous literal data. Resolve
        // these through the symbol table to exercise distant placement.
        let leaf = pipeline.declare_function("distant_leaf").unwrap();
        let data = pipeline
            .module
            .declare_data("distant_data", Linkage::Export, false, false)
            .unwrap();
        let caller = pipeline.declare_function("distant_caller").unwrap();
        let mut ctx = pipeline.module.make_context();
        ctx.func.signature = pipeline.make_func_signature();
        let mut frontend = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut frontend);
        let block = builder.create_block();
        builder.append_block_params_for_function_params(block);
        builder.switch_to_block(block);
        builder.seal_block(block);
        let vmctx = builder.block_params(block)[0];
        let callee = pipeline.module.declare_func_in_func(leaf, builder.func);
        let direct = builder.ins().call(callee, &[vmctx]);
        let direct = builder.inst_results(direct)[0];
        let address = builder.ins().func_addr(types::I64, callee);
        let sig = builder.import_signature(pipeline.make_func_signature());
        let indirect = builder.ins().call_indirect(sig, address, &[vmctx]);
        let indirect = builder.inst_results(indirect)[0];
        let symbol = pipeline.module.declare_data_in_func(data, builder.func);
        let address = builder.ins().symbol_value(types::I64, symbol);
        let value = builder
            .ins()
            .load(types::I64, ir::MemFlags::trusted(), address, 0);
        let result = builder.ins().iadd(direct, indirect);
        let result = builder.ins().iadd(result, value);
        builder.ins().return_(&[result]);
        builder.finalize();
        pipeline.define_function(caller, &mut ctx).unwrap();
        pipeline.finalize().unwrap();
        let caller_ptr = pipeline.get_function_ptr(caller);
        assert!((caller_ptr as usize).abs_diff(leaf_ptr as usize) > i32::MAX as usize);
        assert!((caller_ptr as usize).abs_diff(data_ptr as usize) > i32::MAX as usize);
        // SAFETY: both functions are finalized with the declared signature and
        // their memory remains live until after the last call.
        unsafe {
            let call: unsafe extern "C" fn(usize) -> i64 = std::mem::transmute(caller_ptr);
            assert_eq!(call(0), 103);
            let call_leaf: unsafe extern "C" fn(usize) -> i64 = std::mem::transmute(leaf_ptr);
            assert_eq!(call_leaf(0), 42);
        }
    }

    #[test]
    fn test_declare_define_finalize() {
        let mut pipeline = CodegenPipeline::new(&[]).unwrap();
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

        // SAFETY: ptr is a finalized JIT function pointer with the expected signature.
        let func: unsafe extern "C" fn(usize) -> i64 = unsafe { std::mem::transmute(ptr) };
        // SAFETY: Calling the JIT-compiled function with a dummy vmctx (0).
        let res = unsafe { func(0) };
        assert_eq!(res, 42);
    }

    #[test]
    fn test_build_lambda_registry() {
        let mut pipeline = CodegenPipeline::new(&[]).unwrap();
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
        let mut pipeline = CodegenPipeline::new(&symbols).unwrap();

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
        let local_callee = pipeline.module.declare_func_in_func(callee, builder.func);

        let call = builder.ins().call(local_callee, &[]);
        let res = builder.inst_results(call)[0];
        builder.ins().return_(&[res]);
        builder.finalize();

        pipeline.define_function(func_id, &mut ctx).unwrap();
        pipeline.finalize().unwrap();

        let ptr = pipeline.get_function_ptr(func_id);
        // SAFETY: ptr is a finalized JIT function that calls my_host_fn.
        let func: unsafe extern "C" fn(usize) -> i64 = unsafe { std::mem::transmute(ptr) };
        // SAFETY: Calling the JIT-compiled function with a dummy vmctx (0).
        assert_eq!(unsafe { func(0) }, 123);
    }

    #[test]
    fn intern_name_dedupes_by_name_not_by_call_count() {
        let mut pipeline = CodegenPipeline::new(&[]).unwrap();

        // Simulate multiple case sites in the same function, across
        // multiple "compiles" of that function (e.g. repeated calls in a
        // long-lived server process): the arena must hold one entry per
        // DISTINCT name, not one per call.
        let (ptr_a1, len_a1) = pipeline.intern_name("foo");
        let (ptr_a2, len_a2) = pipeline.intern_name("foo");
        let (ptr_b, len_b) = pipeline.intern_name("bar");
        let (ptr_a3, len_a3) = pipeline.intern_name("foo");

        assert_eq!(pipeline.interned_name_count(), 2);
        assert_eq!(ptr_a1, ptr_a2);
        assert_eq!(ptr_a1, ptr_a3);
        assert_eq!(len_a1, len_a2);
        assert_eq!(len_a1, len_a3);
        assert_ne!(ptr_a1, ptr_b);
        assert_eq!(len_b, "bar".len());

        // Interning ten more distinct names doesn't touch the existing two.
        for i in 0..10 {
            pipeline.intern_name(&format!("fn_{i}"));
        }
        assert_eq!(pipeline.interned_name_count(), 12);
        let (ptr_a4, _) = pipeline.intern_name("foo");
        assert_eq!(ptr_a1, ptr_a4, "rehashing must not move the interned bytes");
    }

    /// Declares, defines and registers a trivial constant-returning function
    /// named `name` in `pipeline`, without finalizing. Shared by the
    /// incremental-registry tests below, which need to control exactly when
    /// `finalize`/`build_lambda_registry` runs relative to registration.
    fn define_trivial_lambda(pipeline: &mut CodegenPipeline, name: &str, ret: i64) -> FuncId {
        let func_id = pipeline.declare_function(name).unwrap();
        let mut ctx = pipeline.module.make_context();
        ctx.func.signature = pipeline.make_func_signature();
        let mut builder_context = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_context);
        let block = builder.create_block();
        builder.append_block_params_for_function_params(block);
        builder.switch_to_block(block);
        builder.seal_block(block);
        let val = builder.ins().iconst(types::I64, ret);
        builder.ins().return_(&[val]);
        builder.finalize();
        pipeline.define_function(func_id, &mut ctx).unwrap();
        pipeline.register_lambda(func_id, name.to_string());
        func_id
    }

    /// Across several "turns" (declare/define/register a few lambdas,
    /// finalize, then read the registry — the same shape `add_function` +
    /// `install_registries` drive per session turn), the incremental
    /// `build_lambda_registry` must contain exactly what a from-scratch
    /// rebuild over the FULL lambda history so far would contain: the
    /// incremental path may only change the cost, never the observable
    /// contents, of a full rebuild.
    #[test]
    fn build_lambda_registry_incremental_matches_full_rebuild() {
        let mut pipeline = CodegenPipeline::new(&[]).unwrap();
        // Independent shadow of `lambda_names: Vec<(FuncId, String)>`'s
        // accumulation, used only to compute the reference full rebuild — it
        // does not touch `pipeline`'s own bookkeeping.
        let mut lambda_names_shadow: Vec<(FuncId, String)> = Vec::new();

        for turn in 0..4usize {
            for i in 0..3usize {
                let name = format!("turn{turn}_lambda{i}");
                let func_id = define_trivial_lambda(&mut pipeline, &name, (turn * 10 + i) as i64);
                lambda_names_shadow.push((func_id, name));
            }
            pipeline.finalize().unwrap();

            // Reference: a full-rebuild algorithm walking the ENTIRE history
            // every turn, computed independently of the incremental path's state.
            let mut full_rebuild: HashMap<usize, String> = HashMap::new();
            for (func_id, name) in &lambda_names_shadow {
                let ptr = pipeline.module.get_finalized_function(*func_id) as usize;
                full_rebuild.insert(ptr, name.clone());
            }

            let incremental = pipeline.build_lambda_registry();
            assert_eq!(
                incremental.len(),
                full_rebuild.len(),
                "turn {turn}: incremental registry size diverged from full rebuild"
            );
            for (ptr, name) in &full_rebuild {
                assert_eq!(
                    incremental.lookup(*ptr),
                    Some(name.as_str()),
                    "turn {turn}: incremental registry missing/mismatched entry for {name}"
                );
            }

            // Simulate the run boundary: the thread-local handle this run
            // would have installed is dropped here (RegistryGuard::drop calls
            // clear_lambda_registry), so the next turn's build_lambda_registry
            // call sees refcount 1 and extends in place rather than cloning.
            drop(incremental);
        }
    }

    /// A call to `build_lambda_registry` with no new lambdas registered
    /// since the last call (i.e. a run that compiles nothing new — the common
    /// case once a session has already declared everything the fragment
    /// needs) must return the SAME accumulated contents, not an empty or
    /// partial registry. This is the amortized-O(1) no-op path.
    #[test]
    fn build_lambda_registry_stable_across_calls_with_no_new_lambdas() {
        let mut pipeline = CodegenPipeline::new(&[]).unwrap();
        let func_id = define_trivial_lambda(&mut pipeline, "only_lambda", 7);
        pipeline.finalize().unwrap();

        let first = pipeline.build_lambda_registry();
        let ptr = pipeline.get_function_ptr(func_id) as usize;
        assert_eq!(first.lookup(ptr), Some("only_lambda"));
        drop(first);

        // No new registrations, no new finalize — just re-read the registry,
        // as a run with nothing new to compile would.
        let second = pipeline.build_lambda_registry();
        assert_eq!(second.len(), 1);
        assert_eq!(second.lookup(ptr), Some("only_lambda"));
    }
}
