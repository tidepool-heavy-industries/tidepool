//! Function-application protocol: `runtime_apply` (non-tail) and
//! `runtime_tail_apply` (tail), sharing the per-function [`FunctionImports`]
//! cache and the `force_and_check_callee` prefix helper.

use crate::emit::{
    heap_force_sig, EmitError, EmitSession, SsaVal, CLOSURE_CODE_PTR_OFFSET, VMCTX_TAIL_ARG_OFFSET,
    VMCTX_TAIL_CALLEE_OFFSET,
};
use cranelift_codegen::ir::{
    condcodes::IntCC, types, AbiParam, BlockArg, FuncRef, InstBuilder, MemFlags, Signature, Value,
};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::{Linkage, Module};

/// ABI signature of `debug_app_check`: `(vmctx, fun_ptr)` (both `I64`)
/// returning `0` (ok) or a poison pointer (`I64`).
fn debug_app_check_sig(call_conv: cranelift_codegen::isa::CallConv) -> Signature {
    let mut sig = Signature::new(call_conv);
    sig.params.push(AbiParam::new(types::I64)); // vmctx
    sig.params.push(AbiParam::new(types::I64)); // fun_ptr
    sig.returns.push(AbiParam::new(types::I64)); // 0 = ok, non-zero = poison
    sig
}

/// ABI signature of `trampoline_resolve`: `(vmctx)` (`I64`) returning the
/// resolved result (`I64`).
fn trampoline_resolve_sig(call_conv: cranelift_codegen::isa::CallConv) -> Signature {
    let mut sig = Signature::new(call_conv);
    sig.params.push(AbiParam::new(types::I64)); // vmctx
    sig.returns.push(AbiParam::new(types::I64)); // result
    sig
}

/// ABI signature of `debug_app_return`: `(vmctx)` (`I64`) returning `I64`
/// (unused by callers — the call is made for its call-depth-decrement side
/// effect only).
fn debug_app_return_sig(call_conv: cranelift_codegen::isa::CallConv) -> Signature {
    let mut sig = Signature::new(call_conv);
    sig.params.push(AbiParam::new(types::I64)); // vmctx
    sig.returns.push(AbiParam::new(types::I64));
    sig
}

/// Per-function cache of the host-fn `FuncRef`s the application protocol
/// imports: `heap_force`, `debug_app_check`, `trampoline_resolve`,
/// `debug_app_return`. Declares each import at most once per function
/// (lazily, on first use) rather than re-declaring it at every `App` node.
///
/// SCOPING, and why it is safe: a `FuncRef` returned by
/// `Module::declare_func_in_func` is only valid **inside the specific
/// Cranelift `Function` it was declared into** — reusing it against a
/// different `Function` is a silently wrong reference (UB at codegen time),
/// not a compile error. This struct is a field of [`EmitSession`], which is
/// reconstructed fresh at each of the four sites in `emit/expr.rs` that build
/// a new Cranelift `Function` (`compile_expr`, `emit_lam`,
/// `emit_thunk_promised`, LetRec phase 3a) and is never reused across them —
/// so the cache's lifetime is created and destroyed exactly with the function
/// it was declared against.
#[derive(Default)]
pub(crate) struct FunctionImports {
    heap_force: Option<FuncRef>,
    debug_app_check: Option<FuncRef>,
    trampoline_resolve: Option<FuncRef>,
    debug_app_return: Option<FuncRef>,
}

impl FunctionImports {
    fn get_or_declare(
        cached: &mut Option<FuncRef>,
        pipeline: &mut crate::pipeline::CodegenPipeline,
        builder: &mut FunctionBuilder,
        name: &str,
        sig: &Signature,
    ) -> Result<FuncRef, EmitError> {
        if let Some(r) = *cached {
            return Ok(r);
        }
        let id = pipeline
            .module
            .declare_function(name, Linkage::Import, sig)
            .map_err(|e| EmitError::CraneliftError(e.to_string()))?;
        let r = pipeline.module.declare_func_in_func(id, builder.func);
        *cached = Some(r);
        Ok(r)
    }

    fn heap_force(
        &mut self,
        pipeline: &mut crate::pipeline::CodegenPipeline,
        builder: &mut FunctionBuilder,
    ) -> Result<FuncRef, EmitError> {
        let sig = heap_force_sig(pipeline.isa.default_call_conv());
        Self::get_or_declare(&mut self.heap_force, pipeline, builder, "heap_force", &sig)
    }

    fn debug_app_check(
        &mut self,
        pipeline: &mut crate::pipeline::CodegenPipeline,
        builder: &mut FunctionBuilder,
    ) -> Result<FuncRef, EmitError> {
        let sig = debug_app_check_sig(pipeline.isa.default_call_conv());
        Self::get_or_declare(
            &mut self.debug_app_check,
            pipeline,
            builder,
            "debug_app_check",
            &sig,
        )
    }

    fn trampoline_resolve(
        &mut self,
        pipeline: &mut crate::pipeline::CodegenPipeline,
        builder: &mut FunctionBuilder,
    ) -> Result<FuncRef, EmitError> {
        let sig = trampoline_resolve_sig(pipeline.isa.default_call_conv());
        Self::get_or_declare(
            &mut self.trampoline_resolve,
            pipeline,
            builder,
            "trampoline_resolve",
            &sig,
        )
    }

    fn debug_app_return(
        &mut self,
        pipeline: &mut crate::pipeline::CodegenPipeline,
        builder: &mut FunctionBuilder,
    ) -> Result<FuncRef, EmitError> {
        let sig = debug_app_return_sig(pipeline.isa.default_call_conv());
        Self::get_or_declare(
            &mut self.debug_app_return,
            pipeline,
            builder,
            "debug_app_return",
            &sig,
        )
    }
}

/// Shared prefix of both `runtime_apply` and `runtime_tail_apply`: force a
/// thunked function value to WHNF, then validate it via `debug_app_check`.
/// Returns `(fun_ptr, check_result)` — `check_result` is `0` for ok, non-zero
/// for poison; callers branch on it themselves.
fn force_and_check_callee(
    sess: &mut EmitSession,
    builder: &mut FunctionBuilder,
    raw_fun_ptr: Value,
) -> Result<(Value, Value), EmitError> {
    let fun_tag = builder
        .ins()
        .load(types::I8, MemFlags::trusted(), raw_fun_ptr, 0);
    let is_thunk = builder.ins().icmp_imm(
        IntCC::Equal,
        fun_tag,
        tidepool_heap::layout::TAG_THUNK as i64,
    );

    let force_fun_block = builder.create_block();
    let fun_ready_block = builder.create_block();
    builder.append_block_param(fun_ready_block, types::I64);

    builder.ins().brif(
        is_thunk,
        force_fun_block,
        &[],
        fun_ready_block,
        &[BlockArg::Value(raw_fun_ptr)],
    );

    builder.switch_to_block(force_fun_block);
    builder.seal_block(force_fun_block);

    let force_ref = sess.function_imports.heap_force(sess.pipeline, builder)?;
    let force_call = builder.ins().call(force_ref, &[sess.vmctx, raw_fun_ptr]);
    let forced_fun = builder.inst_results(force_call)[0];
    builder.declare_value_needs_stack_map(forced_fun);
    builder
        .ins()
        .jump(fun_ready_block, &[BlockArg::Value(forced_fun)]);

    builder.switch_to_block(fun_ready_block);
    builder.seal_block(fun_ready_block);
    let fun_ptr = builder.block_params(fun_ready_block)[0];
    builder.declare_value_needs_stack_map(fun_ptr);

    let check_ref = sess
        .function_imports
        .debug_app_check(sess.pipeline, builder)?;
    let check_inst = builder.ins().call(check_ref, &[sess.vmctx, fun_ptr]);
    let check_result = builder.inst_results(check_inst)[0];

    Ok((fun_ptr, check_result))
}

/// Non-tail function application: force the callee, validate it, call it, and
/// resolve TCO trampoline bounces inline via `merge_block` before returning.
///
/// `raw_fun_ptr`/`arg_ptr` are the already-evaluated function and argument
/// heap pointers (the hylomorphism evaluates App's `fun`/`arg` children before
/// this runs — see `emit/expr.rs`'s `EmitFrame::App` doc). Every heap pointer
/// this function's internal safepoints (`heap_force`/`trampoline_resolve`
/// calls) can see live is marked at its own point of creation — allocation,
/// closure/thunk capture load, or block param — not swept in here; Cranelift's
/// stack-map liveness is a whole-function dataflow analysis (see
/// `cranelift-frontend`'s `declare_value_needs_stack_map`), so a mark made
/// anywhere earlier in this Cranelift `Function` is already visible at every
/// safepoint in it.
pub(crate) fn runtime_apply(
    sess: &mut EmitSession,
    builder: &mut FunctionBuilder,
    raw_fun_ptr: Value,
    arg_ptr: Value,
) -> Result<SsaVal, EmitError> {
    let (fun_ptr, check_result) = force_and_check_callee(sess, builder, raw_fun_ptr)?;

    // If debug_app_check returned non-zero (poison), short-circuit
    let call_block = builder.create_block();
    let merge_block = builder.create_block();
    builder.append_block_param(merge_block, types::I64);

    let is_zero = builder.ins().icmp_imm(IntCC::Equal, check_result, 0);
    builder.ins().brif(
        is_zero,
        call_block,
        &[],
        merge_block,
        &[BlockArg::Value(check_result)],
    );

    // call_block: normal function call
    builder.switch_to_block(call_block);
    builder.seal_block(call_block);

    let code_ptr = builder.ins().load(
        types::I64,
        MemFlags::trusted(),
        fun_ptr,
        CLOSURE_CODE_PTR_OFFSET,
    );

    let mut sig = Signature::new(sess.pipeline.isa.default_call_conv());
    sig.params.push(AbiParam::new(types::I64)); // vmctx
    sig.params.push(AbiParam::new(types::I64)); // self
    sig.params.push(AbiParam::new(types::I64)); // arg
    sig.returns.push(AbiParam::new(types::I64));
    let call_sig = builder.import_signature(sig);

    let inst = builder
        .ins()
        .call_indirect(call_sig, code_ptr, &[sess.vmctx, fun_ptr, arg_ptr]);
    let ret_val = builder.inst_results(inst)[0];

    // TCO null check: if callee returned null, it might be a tail call
    let ret_is_null = builder.ins().icmp_imm(IntCC::Equal, ret_val, 0);
    let null_check_block = builder.create_block();
    let ret_ok_block = builder.create_block();

    builder
        .ins()
        .brif(ret_is_null, null_check_block, &[], ret_ok_block, &[]);

    // ret_ok_block: normal return, jump to merge
    builder.switch_to_block(ret_ok_block);
    builder.seal_block(ret_ok_block);
    builder.ins().jump(merge_block, &[BlockArg::Value(ret_val)]);

    // null_check_block: check if VMContext has a pending tail call
    builder.switch_to_block(null_check_block);
    builder.seal_block(null_check_block);

    let tail_callee = builder.ins().load(
        types::I64,
        MemFlags::trusted(),
        sess.vmctx,
        VMCTX_TAIL_CALLEE_OFFSET,
    );
    let has_tail_call = builder.ins().icmp_imm(IntCC::NotEqual, tail_callee, 0);

    let resolve_block = builder.create_block();
    let null_propagate_block = builder.create_block();

    builder
        .ins()
        .brif(has_tail_call, resolve_block, &[], null_propagate_block, &[]);

    // null_propagate_block: no tail call pending, propagate null (error)
    builder.switch_to_block(null_propagate_block);
    builder.seal_block(null_propagate_block);
    let null_val = builder.ins().iconst(types::I64, 0);
    builder
        .ins()
        .jump(merge_block, &[BlockArg::Value(null_val)]);

    // resolve_block: call trampoline_resolve to execute the pending tail call
    builder.switch_to_block(resolve_block);
    builder.seal_block(resolve_block);

    let resolve_ref = sess
        .function_imports
        .trampoline_resolve(sess.pipeline, builder)?;
    let resolve_inst = builder.ins().call(resolve_ref, &[sess.vmctx]);
    let resolved_val = builder.inst_results(resolve_inst)[0];
    builder.declare_value_needs_stack_map(resolved_val);
    builder
        .ins()
        .jump(merge_block, &[BlockArg::Value(resolved_val)]);

    // merge_block: result from any path. Pair debug_app_check's
    // increment with a decrement here — every exit from this App
    // node (the poison short-circuit and the post-call/post-TCO-
    // resolution path) converges here, so call_depth tracks live
    // nesting instead of a running total.
    builder.switch_to_block(merge_block);
    builder.seal_block(merge_block);
    let merged_val = builder.block_params(merge_block)[0];
    builder.declare_value_needs_stack_map(merged_val);

    let return_ref = sess
        .function_imports
        .debug_app_return(sess.pipeline, builder)?;
    builder.ins().call(return_ref, &[sess.vmctx]);

    Ok(SsaVal::HeapPtr(merged_val))
}

/// Tail function application: hand off to the trampoline rather than calling
/// directly. Same mark-at-creation coverage argument as `runtime_apply` above
/// applies here.
pub(crate) fn runtime_tail_apply(
    sess: &mut EmitSession,
    builder: &mut FunctionBuilder,
    raw_fun_ptr: Value,
    arg_ptr: Value,
) -> Result<SsaVal, EmitError> {
    let (fun_ptr, check_result) = force_and_check_callee(sess, builder, raw_fun_ptr)?;

    // If debug_app_check returned non-zero (poison/error), return it directly
    let store_block = builder.create_block();
    let poison_block = builder.create_block();

    let is_zero = builder.ins().icmp_imm(IntCC::Equal, check_result, 0);
    builder
        .ins()
        .brif(is_zero, store_block, &[], poison_block, &[]);

    // poison_block: return poison (error already set by debug_app_check)
    builder.switch_to_block(poison_block);
    builder.seal_block(poison_block);
    builder.ins().return_(&[check_result]);

    // store_block: store callee+arg to VMContext, return null
    builder.switch_to_block(store_block);
    builder.seal_block(store_block);

    builder.ins().store(
        MemFlags::trusted(),
        fun_ptr,
        sess.vmctx,
        VMCTX_TAIL_CALLEE_OFFSET,
    );
    builder.ins().store(
        MemFlags::trusted(),
        arg_ptr,
        sess.vmctx,
        VMCTX_TAIL_ARG_OFFSET,
    );

    let null_val = builder.ins().iconst(types::I64, 0);
    builder.ins().return_(&[null_val]);

    let dead_block = builder.create_block();
    builder.switch_to_block(dead_block);
    builder.seal_block(dead_block);

    let dummy = builder.ins().iconst(types::I64, 0);
    Ok(SsaVal::HeapPtr(dummy))
}
