use cranelift_codegen::ir;
use cranelift_codegen::isa::TargetIsa;
use cranelift_codegen::settings::{self, Configurable};
use cranelift_codegen::Context;
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module};
use std::collections::BTreeMap;
use std::mem::ManuallyDrop;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

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
    /// Opt-in pre-compilation IR for structural allocation contract tests.
    #[cfg(test)]
    pub(crate) emitted_ir: Option<BTreeMap<FuncId, ir::Function>>,
    /// The JIT module that manages executable memory.
    ///
    /// This field is public as an **escape hatch** for advanced use cases and tests
    /// that need direct access to Cranelift's `JITModule`. Most users should prefer
    /// the safe wrapper methods on `CodegenPipeline`
    /// instead of calling into `module` directly.
    pub module: OwnedJitModule,
    /// Target ISA (needed for Context::compile).
    pub isa: Arc<dyn TargetIsa>,
    /// Stack map registry populated during compilation.
    pub stack_maps: StackMapRegistry,
    /// Pending stack maps waiting for finalization to get base pointers.
    /// Stores (func_id, func_size, finalized-frame reserve, raw_maps).
    pending_stack_maps: Vec<(FuncId, u32, usize, Vec<RawStackMap>)>,
    /// Largest finalized native frame reserve in this module. The reserve is
    /// retained only after `finalize`, so callers cannot preflight against a
    /// frame whose code was not made callable.
    native_frame_maximum: usize,
    /// Boxed-literal wrapper constructor ids (I#/W#/C#/F#/D#) for this compile,
    /// set by the JIT entry point from the DataConTable. Transported here so
    /// `compile_expr` can stamp it onto every `EmitSession` without threading
    /// the table through its signature. Defaults to empty (no wrapper
    /// tolerance), which preserves behavior for direct test callers.
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
    /// Session-lifetime sum of machine-code bytes Cranelift emitted for the
    /// functions this pipeline defined. Read the same snapshot-before/
    /// diff-after way as [`Self::functions_defined`]; it is the size half of
    /// the same question — how much executable memory one turn cost.
    code_bytes: u64,
}

impl CodegenPipeline {
    /// Create a new CodegenPipeline with default x86-64 settings.
    ///
    /// `symbols` is a list of (name, pointer) pairs for host functions
    /// that JIT code can call (e.g.).
    pub fn new(symbols: &[(&str, *const u8)]) -> Result<Self, PipelineError> {
        let mut flag_builder = settings::builder();
        flag_builder
            .set("enable_multi_ret_implicit_sret", "true")
            .map_err(|e| PipelineError::Init(format!("set implicit result transport: {e}")))?;
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
        // Cranelift's own default is "true", which re-verifies every compiled
        // function's IR on every build. That's worth paying for in debug
        // builds (catches a malformed IR construction immediately, at its
        // source, instead of as a much harder to diagnose miscompile or
        // crash downstream), but it's dead weight in release builds where
        // IR construction is already exercised under debug_assertions in CI.
        // TIDEPOOL_CRANELIFT_VERIFY=1 forces it back on in a release build
        // (e.g. to bisect a release-only miscompile).
        let verify = if cfg!(debug_assertions) {
            true
        } else {
            std::env::var_os("TIDEPOOL_CRANELIFT_VERIFY").is_some_and(|v| v == "1")
        };
        flag_builder
            .set("enable_verifier", if verify { "true" } else { "false" })
            .map_err(|e| PipelineError::Init(format!("set enable_verifier: {e}")))?;

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
            #[cfg(test)]
            emitted_ir: None,
            module,
            isa,
            stack_maps: StackMapRegistry::new(),
            pending_stack_maps: Vec::new(),
            native_frame_maximum: 0,
            functions_defined: 0,
            blocks_emitted: 0,
            code_bytes: 0,
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

    /// Session-lifetime machine-code bytes emitted. See the `code_bytes`
    /// field doc for how to read a delta.
    pub fn code_bytes(&self) -> u64 {
        self.code_bytes
    }

    /// Largest native stack reserve among finalized functions.
    ///
    /// Cranelift's finalized machine metadata exposes the active frame from
    /// the current SP through the frame pointer. On the pinned x86-64 SysV
    /// target, the ABI setup area is 16 bytes (saved RBP plus return address),
    /// so the reserve includes both the active frame—including outgoing
    /// argument space—and that setup area before another generated call.
    pub fn native_frame_maximum(&self) -> usize {
        self.native_frame_maximum
    }

    /// Declare a function with a caller-supplied checked signature.
    ///
    /// Prepared-program emitters use this path so definitions, calls and C
    /// adapters all consume the same [`crate::entry_abi::EntryAbi`] lowering.
    pub fn declare_function_with_signature(
        &mut self,
        name: &str,
        linkage: Linkage,
        signature: &ir::Signature,
    ) -> Result<FuncId, PipelineError> {
        self.ensure_usable()?;
        self.module
            .declare_function(name, linkage, signature)
            .map_err(|e| PipelineError::Declaration(format!("failed to declare `{name}`: {e}")))
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
        #[cfg(test)]
        if let Some(functions) = &mut self.emitted_ir {
            functions.insert(func_id, ctx.func.clone());
        }
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

        // Cranelift's call-site table is the authoritative inventory of exact
        // return PCs. `user_stack_maps()` contains only calls with at least one
        // live declared GC value, so using it alone mistakes a valid zero-root
        // safepoint for missing metadata. Join the two tables by exact return
        // offset and retain an empty root list for zero-root calls.
        let compiled = ctx.compiled_code().ok_or_else(|| {
            PipelineError::Compilation("compiled_code missing after define_function".into())
        })?;
        let func_size = compiled.code_buffer().len() as u32;
        let frame_size = compiled
            .buffer
            .frame_layout()
            .ok_or_else(|| PipelineError::Compilation("Cranelift omitted frame layout".into()))?
            .frame_to_fp_offset;
        // The prepared target is currently pinned to x86-64 SysV. This is the
        // setup area Cranelift's x64 FrameLayout reserves for RBP and the
        // return address; frame_to_fp_offset already includes outgoing args,
        // fixed storage, and callee-save clobbers.
        const X86_64_SETUP_AREA: usize = 16;
        let native_frame_reserve = (frame_size as usize)
            .checked_add(X86_64_SETUP_AREA)
            .ok_or_else(|| PipelineError::Compilation("native frame reserve overflow".into()))?;
        let mut rooted_maps: BTreeMap<u32, (u32, Vec<RawStackMapEntry>)> = compiled
            .buffer
            .user_stack_maps()
            .iter()
            .map(|(offset, span, usm)| {
                let entries: Vec<_> = usm
                    .entries()
                    .map(|(ty, offset)| RawStackMapEntry { ty, offset })
                    .collect();
                (*offset, (*span, entries))
            })
            .collect();
        let mut raw_maps = Vec::new();
        for call_site in compiled.buffer.call_sites() {
            let entries = match rooted_maps.remove(&call_site.ret_addr) {
                Some((stack_map_frame_size, entries)) => {
                    if stack_map_frame_size != frame_size {
                        return Err(PipelineError::Compilation(format!(
                            "Cranelift call site at {:#x} disagrees on frame size: call-site {frame_size}, stack-map {stack_map_frame_size}",
                            call_site.ret_addr
                        )));
                    }
                    entries
                }
                None => Vec::new(),
            };
            raw_maps.push(RawStackMap {
                code_offset: call_site.ret_addr,
                frame_size,
                entries,
            });
        }
        if let Some((&code_offset, _)) = rooted_maps.first_key_value() {
            return Err(PipelineError::Compilation(format!(
                "Cranelift stack map at {code_offset:#x} has no matching call site"
            )));
        }

        self.pending_stack_maps
            .push((func_id, func_size, native_frame_reserve, raw_maps));
        self.functions_defined += 1;
        self.blocks_emitted += ctx.func.layout.blocks().count() as u64;
        self.code_bytes += u64::from(func_size);
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
        for (func_id, func_size, frame_reserve, raw_maps) in pending {
            let base_ptr = self.module.get_finalized_function(func_id) as usize;
            self.stack_maps.register(base_ptr, func_size, &raw_maps);
            self.native_frame_maximum = self.native_frame_maximum.max(frame_reserve);
        }
        self.compilation_state = CompilationState::Ready;
        Ok(())
    }

    /// Get the callable function pointer after finalization. The pointer remains
    /// valid only while this pipeline's module owner is alive.
    pub fn get_function_ptr(&self, func_id: FuncId) -> *const u8 {
        self.module.get_finalized_function(func_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cranelift_codegen::ir::{types, AbiParam, InstBuilder};
    use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};

    fn test_signature(pipeline: &CodegenPipeline) -> ir::Signature {
        let mut signature = ir::Signature::new(pipeline.isa.default_call_conv());
        signature.params.push(AbiParam::new(types::I64));
        signature.returns.push(AbiParam::new(types::I64));
        signature
    }

    fn declare_test_function(
        pipeline: &mut CodegenPipeline,
        name: &str,
    ) -> Result<FuncId, PipelineError> {
        let signature = test_signature(pipeline);
        pipeline.declare_function_with_signature(name, Linkage::Export, &signature)
    }

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
        }
        for fail in [Failure::None, Failure::Definition] {
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
        let leaf = declare_test_function(&mut pipeline, "distant_leaf").unwrap();
        let data = pipeline
            .module
            .declare_data("distant_data", Linkage::Export, false, false)
            .unwrap();
        let caller = declare_test_function(&mut pipeline, "distant_caller").unwrap();
        let mut ctx = pipeline.module.make_context();
        ctx.func.signature = test_signature(&pipeline);
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
        let sig = builder.import_signature(test_signature(&pipeline));
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
        let func_id = declare_test_function(&mut pipeline, "test_fn").unwrap();

        let mut ctx = pipeline.module.make_context();
        ctx.func.signature = test_signature(&pipeline);

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
    fn test_host_fn_symbols_integration() {
        extern "C" fn my_host_fn() -> i64 {
            123
        }
        let symbols = [("my_host_fn", my_host_fn as *const u8)];
        let mut pipeline = CodegenPipeline::new(&symbols).unwrap();

        let func_id = declare_test_function(&mut pipeline, "call_host").unwrap();
        let mut ctx = pipeline.module.make_context();
        ctx.func.signature = test_signature(&pipeline);

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
    /// Declares and defines a trivial constant-returning function without finalizing.
    fn define_trivial_lambda(pipeline: &mut CodegenPipeline, name: &str, ret: i64) -> FuncId {
        let func_id = declare_test_function(pipeline, name).unwrap();
        let mut ctx = pipeline.module.make_context();
        ctx.func.signature = test_signature(pipeline);
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
        func_id
    }
}
