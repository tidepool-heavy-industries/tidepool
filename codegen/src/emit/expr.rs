use crate::pipeline::CodegenPipeline;
use crate::alloc::emit_alloc_fast_path;
use crate::emit::*;
use core_repr::*;
use core_heap::layout;
use cranelift_codegen::ir::{self, types, AbiParam, InstBuilder, MemFlags, Value, Signature, UserFuncName};
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{Module, Linkage, FuncId};

/// Compile a CoreExpr into a JIT function. Returns the FuncId.
/// The compiled function has signature: (vmctx: i64) -> i64
/// It returns a heap pointer to the result.
pub fn compile_expr(
    pipeline: &mut CodegenPipeline,
    tree: &CoreExpr,
    name: &str,
) -> Result<FuncId, EmitError> {
    let sig = pipeline.make_func_signature();
    let func_id = pipeline.module.declare_function(name, Linkage::Export, &sig)
        .map_err(|e| EmitError::CraneliftError(e.to_string()))?;

    let mut ctx = Context::new();
    ctx.func.signature = sig;
    ctx.func.name = UserFuncName::default();

    let mut fb_ctx = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut ctx.func, &mut fb_ctx);

    let entry_block = builder.create_block();
    builder.append_block_params_for_function_params(entry_block);
    builder.switch_to_block(entry_block);
    builder.seal_block(entry_block);

    let vmctx = builder.block_params(entry_block)[0];

    let mut gc_sig = Signature::new(pipeline.isa.default_call_conv());
    gc_sig.params.push(AbiParam::new(types::I64));
    let gc_sig_ref = builder.import_signature(gc_sig);

    let mut emit_ctx = EmitContext::new(name.to_string());
    let result = emit_ctx.emit_node(pipeline, &mut builder, vmctx, gc_sig_ref, tree, tree.nodes.len() - 1)?;
    let ret = ensure_heap_ptr(&mut builder, vmctx, gc_sig_ref, result);

    builder.ins().return_(&[ret]);
    builder.finalize();

    pipeline.define_function(func_id, &mut ctx);

    Ok(func_id)
}

impl EmitContext {
    pub fn emit_node(
        &mut self,
        pipeline: &mut CodegenPipeline,
        builder: &mut FunctionBuilder,
        vmctx: Value,
        gc_sig: ir::SigRef,
        tree: &CoreExpr,
        idx: usize,
    ) -> Result<SsaVal, EmitError> {
        match &tree.nodes[idx] {
            CoreFrame::Lit(lit) => emit_lit(builder, vmctx, gc_sig, lit),
            CoreFrame::Var(vid) => {
                self.env.get(vid).copied().ok_or(EmitError::UnboundVariable(*vid))
            }
            CoreFrame::Con { tag, fields } => {
                let mut field_vals = Vec::new();
                for &f_idx in fields {
                    let val = self.emit_node(pipeline, builder, vmctx, gc_sig, tree, f_idx)?;
                    field_vals.push(ensure_heap_ptr(builder, vmctx, gc_sig, val));
                }

                let num_fields = field_vals.len();
                let size = 24 + 8 * num_fields as u64;
                let ptr = emit_alloc_fast_path(builder, vmctx, size, gc_sig);

                let tag_val = builder.ins().iconst(types::I8, layout::TAG_CON as i64);
                builder.ins().store(MemFlags::trusted(), tag_val, ptr, 0);
                let size_val = builder.ins().iconst(types::I16, size as i64);
                builder.ins().store(MemFlags::trusted(), size_val, ptr, 1);

                let con_tag_val = builder.ins().iconst(types::I64, tag.0 as i64);
                builder.ins().store(MemFlags::trusted(), con_tag_val, ptr, CON_TAG_OFFSET);
                let num_fields_val = builder.ins().iconst(types::I16, num_fields as i64);
                builder.ins().store(MemFlags::trusted(), num_fields_val, ptr, CON_NUM_FIELDS_OFFSET);

                for (i, field_val) in field_vals.into_iter().enumerate() {
                    builder.ins().store(MemFlags::trusted(), field_val, ptr, CON_FIELDS_START + 8 * i as i32);
                }

                builder.declare_value_needs_stack_map(ptr);
                Ok(SsaVal::HeapPtr(ptr))
            }
            CoreFrame::PrimOp { op, args } => {
                let mut arg_vals = Vec::new();
                for &a_idx in args {
                    arg_vals.push(self.emit_node(pipeline, builder, vmctx, gc_sig, tree, a_idx)?);
                }
                primop::emit_primop(builder, op, &arg_vals)
            }
            CoreFrame::App { fun, arg } => {
                let fun_val = self.emit_node(pipeline, builder, vmctx, gc_sig, tree, *fun)?;
                let arg_val = self.emit_node(pipeline, builder, vmctx, gc_sig, tree, *arg)?;
                let fun_ptr = fun_val.value();
                let arg_ptr = ensure_heap_ptr(builder, vmctx, gc_sig, arg_val);

                let code_ptr = builder.ins().load(types::I64, MemFlags::trusted(), fun_ptr, CLOSURE_CODE_PTR_OFFSET);

                let mut sig = Signature::new(pipeline.isa.default_call_conv());
                sig.params.push(AbiParam::new(types::I64)); // vmctx
                sig.params.push(AbiParam::new(types::I64)); // self
                sig.params.push(AbiParam::new(types::I64)); // arg
                sig.returns.push(AbiParam::new(types::I64));
                let call_sig = builder.import_signature(sig);

                let inst = builder.ins().call_indirect(call_sig, code_ptr, &[vmctx, fun_ptr, arg_ptr]);
                let ret_val = builder.inst_results(inst)[0];
                builder.declare_value_needs_stack_map(ret_val);
                Ok(SsaVal::HeapPtr(ret_val))
            }
            CoreFrame::Lam { binder, body } => {
                let body_tree = tree.extract_subtree(*body);
                let mut fvs = core_repr::free_vars::free_vars(&body_tree);
                fvs.remove(binder);

                let mut sorted_fvs: Vec<VarId> = fvs.into_iter().filter(|v| self.env.contains_key(v)).collect();
                sorted_fvs.sort_by_key(|v| v.0);

                let captures: Vec<(VarId, SsaVal)> = sorted_fvs.iter().map(|v| (*v, self.env[v])).collect();

                let lambda_name = self.next_lambda_name();
                let mut closure_sig = Signature::new(pipeline.isa.default_call_conv());
                closure_sig.params.push(AbiParam::new(types::I64)); // vmctx
                closure_sig.params.push(AbiParam::new(types::I64)); // self
                closure_sig.params.push(AbiParam::new(types::I64)); // arg
                closure_sig.returns.push(AbiParam::new(types::I64));

                let lambda_func_id = pipeline.module.declare_function(&lambda_name, Linkage::Local, &closure_sig)
                    .map_err(|e| EmitError::CraneliftError(e.to_string()))?;

                let mut inner_ctx = Context::new();
                inner_ctx.func.signature = closure_sig;
                inner_ctx.func.name = UserFuncName::default();

                let mut inner_fb_ctx = FunctionBuilderContext::new();
                let mut inner_builder = FunctionBuilder::new(&mut inner_ctx.func, &mut inner_fb_ctx);
                let inner_block = inner_builder.create_block();
                inner_builder.append_block_params_for_function_params(inner_block);
                inner_builder.switch_to_block(inner_block);
                inner_builder.seal_block(inner_block);

                let inner_vmctx = inner_builder.block_params(inner_block)[0];
                let closure_self = inner_builder.block_params(inner_block)[1];
                let arg_param = inner_builder.block_params(inner_block)[2];

                inner_builder.declare_value_needs_stack_map(closure_self);
                inner_builder.declare_value_needs_stack_map(arg_param);

                let mut inner_gc_sig = Signature::new(pipeline.isa.default_call_conv());
                inner_gc_sig.params.push(AbiParam::new(types::I64));
                let inner_gc_sig_ref = inner_builder.import_signature(inner_gc_sig);

                let mut inner_emit = EmitContext::new(self.prefix.clone());
                inner_emit.lambda_counter = self.lambda_counter;

                inner_emit.env.insert(*binder, SsaVal::HeapPtr(arg_param));

                for (i, (var_id, _)) in captures.iter().enumerate() {
                    let offset = CLOSURE_CAPTURED_START + 8 * i as i32;
                    let val = inner_builder.ins().load(types::I64, MemFlags::trusted(), closure_self, offset);
                    inner_builder.declare_value_needs_stack_map(val);
                    inner_emit.env.insert(*var_id, SsaVal::HeapPtr(val));
                }

                let body_root = body_tree.nodes.len() - 1;
                let body_result = inner_emit.emit_node(pipeline, &mut inner_builder, inner_vmctx, inner_gc_sig_ref, &body_tree, body_root)?;
                let ret_val = ensure_heap_ptr(&mut inner_builder, inner_vmctx, inner_gc_sig_ref, body_result);

                inner_builder.ins().return_(&[ret_val]);
                inner_builder.finalize();

                self.lambda_counter = inner_emit.lambda_counter;
                pipeline.define_function(lambda_func_id, &mut inner_ctx);

                let func_ref = pipeline.module.declare_func_in_func(lambda_func_id, builder.func);
                let code_ptr = builder.ins().func_addr(types::I64, func_ref);

                let num_captures = captures.len();
                let closure_size = 24 + 8 * num_captures as u64;
                let closure_ptr = emit_alloc_fast_path(builder, vmctx, closure_size, gc_sig);

                let tag_val = builder.ins().iconst(types::I8, layout::TAG_CLOSURE as i64);
                builder.ins().store(MemFlags::trusted(), tag_val, closure_ptr, 0);
                let size_val = builder.ins().iconst(types::I16, closure_size as i64);
                builder.ins().store(MemFlags::trusted(), size_val, closure_ptr, 1);

                builder.ins().store(MemFlags::trusted(), code_ptr, closure_ptr, CLOSURE_CODE_PTR_OFFSET);
                let num_cap_val = builder.ins().iconst(types::I16, num_captures as i64);
                builder.ins().store(MemFlags::trusted(), num_cap_val, closure_ptr, CLOSURE_NUM_CAPTURED_OFFSET);

                for (i, (_, ssaval)) in captures.iter().enumerate() {
                    let cap_val = ensure_heap_ptr(builder, vmctx, gc_sig, *ssaval);
                    let offset = CLOSURE_CAPTURED_START + 8 * i as i32;
                    builder.ins().store(MemFlags::trusted(), cap_val, closure_ptr, offset);
                }

                builder.declare_value_needs_stack_map(closure_ptr);
                Ok(SsaVal::HeapPtr(closure_ptr))
            }
            CoreFrame::LetNonRec { binder, rhs, body } => {
                let rhs_val = self.emit_node(pipeline, builder, vmctx, gc_sig, tree, *rhs)?;
                self.env.insert(*binder, rhs_val);
                let body_val = self.emit_node(pipeline, builder, vmctx, gc_sig, tree, *body)?;
                self.env.remove(binder);
                Ok(body_val)
            }
            CoreFrame::LetRec { bindings, body } => {
                for (_, rhs_idx) in bindings {
                    if !matches!(tree.nodes[*rhs_idx], CoreFrame::Lam { .. }) {
                        return Err(EmitError::NotYetImplemented("LetRec with non-lambda RHS".into()));
                    }
                }

                let mut pre_allocs = Vec::new();
                for (binder, rhs_idx) in bindings {
                    // This is Lam, we need the body subtree but let's just use it to find captures
                    let (lam_binder, lam_body) = match &tree.nodes[*rhs_idx] {
                        CoreFrame::Lam { binder, body } => (*binder, *body),
                        _ => unreachable!(),
                    };
                    let lam_body_tree = tree.extract_subtree(lam_body);
                    let mut fvs = core_repr::free_vars::free_vars(&lam_body_tree);
                    fvs.remove(&lam_binder);
                    
                    // Captures include other letrec binders + outer env
                    let mut sorted_fvs: Vec<VarId> = fvs.into_iter().filter(|v| {
                        self.env.contains_key(v) || bindings.iter().any(|(b, _)| b == v)
                    }).collect();
                    sorted_fvs.sort_by_key(|v| v.0);
                    
                    let num_captures = sorted_fvs.len();
                    let closure_size = 24 + 8 * num_captures as u64;
                    let closure_ptr = emit_alloc_fast_path(builder, vmctx, closure_size, gc_sig);
                    
                    let tag_val = builder.ins().iconst(types::I8, layout::TAG_CLOSURE as i64);
                    builder.ins().store(MemFlags::trusted(), tag_val, closure_ptr, 0);
                    let size_val = builder.ins().iconst(types::I16, closure_size as i64);
                    builder.ins().store(MemFlags::trusted(), size_val, closure_ptr, 1);
                    let num_cap_val = builder.ins().iconst(types::I16, num_captures as i64);
                    builder.ins().store(MemFlags::trusted(), num_cap_val, closure_ptr, CLOSURE_NUM_CAPTURED_OFFSET);
                    
                    builder.declare_value_needs_stack_map(closure_ptr);
                    pre_allocs.push((*binder, closure_ptr, sorted_fvs, *rhs_idx));
                }

                // Bind all to their pre-allocated pointers
                for (binder, ptr, _, _) in &pre_allocs {
                    self.env.insert(*binder, SsaVal::HeapPtr(*ptr));
                }

                // Compile bodies and fill closures
                for (_, closure_ptr, sorted_fvs, rhs_idx) in pre_allocs {
                    let (lam_binder, lam_body) = match &tree.nodes[rhs_idx] {
                        CoreFrame::Lam { binder, body } => (*binder, *body),
                        _ => unreachable!(),
                    };
                    let lam_body_tree = tree.extract_subtree(lam_body);
                    
                    let captures: Vec<(VarId, SsaVal)> = sorted_fvs.iter().map(|v| (*v, self.env[v])).collect();
                    
                    let lambda_name = self.next_lambda_name();
                    let mut closure_sig = Signature::new(pipeline.isa.default_call_conv());
                    closure_sig.params.push(AbiParam::new(types::I64));
                    closure_sig.params.push(AbiParam::new(types::I64));
                    closure_sig.params.push(AbiParam::new(types::I64));
                    closure_sig.returns.push(AbiParam::new(types::I64));

                    let lambda_func_id = pipeline.module.declare_function(&lambda_name, Linkage::Local, &closure_sig)
                        .map_err(|e| EmitError::CraneliftError(e.to_string()))?;

                    let mut inner_ctx = Context::new();
                    inner_ctx.func.signature = closure_sig;
                    inner_ctx.func.name = UserFuncName::default();

                    let mut inner_fb_ctx = FunctionBuilderContext::new();
                    let mut inner_builder = FunctionBuilder::new(&mut inner_ctx.func, &mut inner_fb_ctx);
                    let inner_block = inner_builder.create_block();
                    inner_builder.append_block_params_for_function_params(inner_block);
                    inner_builder.switch_to_block(inner_block);
                    inner_builder.seal_block(inner_block);

                    let inner_vmctx = inner_builder.block_params(inner_block)[0];
                    let inner_self = inner_builder.block_params(inner_block)[1];
                    let inner_arg = inner_builder.block_params(inner_block)[2];

                    inner_builder.declare_value_needs_stack_map(inner_self);
                    inner_builder.declare_value_needs_stack_map(inner_arg);

                    let mut inner_gc_sig = Signature::new(pipeline.isa.default_call_conv());
                    inner_gc_sig.params.push(AbiParam::new(types::I64));
                    let inner_gc_sig_ref = inner_builder.import_signature(inner_gc_sig);

                    let mut inner_emit = EmitContext::new(self.prefix.clone());
                    inner_emit.lambda_counter = self.lambda_counter;
                    inner_emit.env.insert(lam_binder, SsaVal::HeapPtr(inner_arg));

                    for (i, (var_id, _)) in captures.iter().enumerate() {
                        let offset = CLOSURE_CAPTURED_START + 8 * i as i32;
                        let val = inner_builder.ins().load(types::I64, MemFlags::trusted(), inner_self, offset);
                        inner_builder.declare_value_needs_stack_map(val);
                        inner_emit.env.insert(*var_id, SsaVal::HeapPtr(val));
                    }

                    let body_root = lam_body_tree.nodes.len() - 1;
                    let body_result = inner_emit.emit_node(pipeline, &mut inner_builder, inner_vmctx, inner_gc_sig_ref, &lam_body_tree, body_root)?;
                    let ret_val = ensure_heap_ptr(&mut inner_builder, inner_vmctx, inner_gc_sig_ref, body_result);

                    inner_builder.ins().return_(&[ret_val]);
                    inner_builder.finalize();
                    self.lambda_counter = inner_emit.lambda_counter;
                    pipeline.define_function(lambda_func_id, &mut inner_ctx);

                    let func_ref = pipeline.module.declare_func_in_func(lambda_func_id, builder.func);
                    let code_ptr = builder.ins().func_addr(types::I64, func_ref);
                    builder.ins().store(MemFlags::trusted(), code_ptr, closure_ptr, CLOSURE_CODE_PTR_OFFSET);

                    for (i, (_, ssaval)) in captures.into_iter().enumerate() {
                        let cap_val = ensure_heap_ptr(builder, vmctx, gc_sig, ssaval);
                        let offset = CLOSURE_CAPTURED_START + 8 * i as i32;
                        builder.ins().store(MemFlags::trusted(), cap_val, closure_ptr, offset);
                    }
                }

                let body_val = self.emit_node(pipeline, builder, vmctx, gc_sig, tree, *body)?;
                for (binder, _) in bindings {
                    self.env.remove(binder);
                }
                Ok(body_val)
            }
            CoreFrame::Case { .. } => Err(EmitError::NotYetImplemented("Case".into())),
            CoreFrame::Join { .. } => Err(EmitError::NotYetImplemented("Join".into())),
            CoreFrame::Jump { .. } => Err(EmitError::NotYetImplemented("Jump".into())),
        }
    }
}

fn emit_lit(
    builder: &mut FunctionBuilder,
    vmctx: Value,
    gc_sig: ir::SigRef,
    lit: &Literal,
) -> Result<SsaVal, EmitError> {
    let ptr = emit_alloc_fast_path(builder, vmctx, LIT_TOTAL_SIZE, gc_sig);

    let tag = builder.ins().iconst(types::I8, layout::TAG_LIT as i64);
    builder.ins().store(MemFlags::trusted(), tag, ptr, 0);
    let size = builder.ins().iconst(types::I16, LIT_TOTAL_SIZE as i64);
    builder.ins().store(MemFlags::trusted(), size, ptr, 1);

    match lit {
        Literal::LitInt(n) => {
            let lit_tag = builder.ins().iconst(types::I8, LIT_TAG_INT);
            builder.ins().store(MemFlags::trusted(), lit_tag, ptr, LIT_TAG_OFFSET);
            let val = builder.ins().iconst(types::I64, *n);
            builder.ins().store(MemFlags::trusted(), val, ptr, LIT_VALUE_OFFSET);
            builder.declare_value_needs_stack_map(ptr);
            Ok(SsaVal::HeapPtr(ptr))
        }
        Literal::LitWord(n) => {
            let lit_tag = builder.ins().iconst(types::I8, LIT_TAG_WORD);
            builder.ins().store(MemFlags::trusted(), lit_tag, ptr, LIT_TAG_OFFSET);
            let val = builder.ins().iconst(types::I64, *n as i64);
            builder.ins().store(MemFlags::trusted(), val, ptr, LIT_VALUE_OFFSET);
            builder.declare_value_needs_stack_map(ptr);
            Ok(SsaVal::HeapPtr(ptr))
        }
        Literal::LitChar(c) => {
            let lit_tag = builder.ins().iconst(types::I8, LIT_TAG_CHAR);
            builder.ins().store(MemFlags::trusted(), lit_tag, ptr, LIT_TAG_OFFSET);
            let val = builder.ins().iconst(types::I64, *c as i64);
            builder.ins().store(MemFlags::trusted(), val, ptr, LIT_VALUE_OFFSET);
            builder.declare_value_needs_stack_map(ptr);
            Ok(SsaVal::HeapPtr(ptr))
        }
        Literal::LitFloat(bits) => {
            let lit_tag = builder.ins().iconst(types::I8, LIT_TAG_FLOAT);
            builder.ins().store(MemFlags::trusted(), lit_tag, ptr, LIT_TAG_OFFSET);
            let val = builder.ins().iconst(types::I64, *bits as i64);
            builder.ins().store(MemFlags::trusted(), val, ptr, LIT_VALUE_OFFSET);
            builder.declare_value_needs_stack_map(ptr);
            Ok(SsaVal::HeapPtr(ptr))
        }
        Literal::LitDouble(bits) => {
            let lit_tag = builder.ins().iconst(types::I8, LIT_TAG_DOUBLE);
            builder.ins().store(MemFlags::trusted(), lit_tag, ptr, LIT_TAG_OFFSET);
            let val = builder.ins().iconst(types::I64, *bits as i64);
            builder.ins().store(MemFlags::trusted(), val, ptr, LIT_VALUE_OFFSET);
            builder.declare_value_needs_stack_map(ptr);
            Ok(SsaVal::HeapPtr(ptr))
        }
        Literal::LitString(_) => Err(EmitError::NotYetImplemented("LitString".into())),
    }
}

fn ensure_heap_ptr(
    builder: &mut FunctionBuilder,
    vmctx: Value,
    gc_sig: ir::SigRef,
    val: SsaVal,
) -> Value {
    match val {
        SsaVal::HeapPtr(v) => v,
        SsaVal::Raw(v, lit_tag) => {
            let ptr = emit_alloc_fast_path(builder, vmctx, LIT_TOTAL_SIZE, gc_sig);
            let tag = builder.ins().iconst(types::I8, layout::TAG_LIT as i64);
            builder.ins().store(MemFlags::trusted(), tag, ptr, 0);
            let size = builder.ins().iconst(types::I16, LIT_TOTAL_SIZE as i64);
            builder.ins().store(MemFlags::trusted(), size, ptr, 1);
            let lit_tag_val = builder.ins().iconst(types::I8, lit_tag);
            builder.ins().store(MemFlags::trusted(), lit_tag_val, ptr, LIT_TAG_OFFSET);
            builder.ins().store(MemFlags::trusted(), v, ptr, LIT_VALUE_OFFSET);
            builder.declare_value_needs_stack_map(ptr);
            ptr
        }
    }
}