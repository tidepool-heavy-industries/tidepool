use crate::pipeline::CodegenPipeline;
use crate::alloc::emit_alloc_fast_path;
use crate::emit::*;
use tidepool_repr::*;
use tidepool_heap::layout;
use cranelift_codegen::ir::{self, types, AbiParam, InstBuilder, MemFlags, Value, Signature, UserFuncName};
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{Module, Linkage, FuncId, DataDescription};

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
        mut idx: usize,
    ) -> Result<SsaVal, EmitError> {
        // Iterative tail-position loop: LetNonRec/LetRec body is in tail position,
        // so we iterate instead of recursing to avoid stack overflow on deep let-chains.
        let mut let_cleanup: Vec<LetCleanup> = Vec::new();
        let result = loop {
        match &tree.nodes[idx] {
            CoreFrame::Lit(Literal::LitString(bytes)) => {
                break emit_lit_string(pipeline, builder, vmctx, gc_sig, bytes, &mut self.lambda_counter);
            }
            CoreFrame::Lit(lit) => break emit_lit(builder, vmctx, gc_sig, lit),
            CoreFrame::Var(vid) => {
                break match self.env.get(vid).copied() {
                    Some(v) => Ok(v),
                    None => {
                        // Check for well-known runtime error VarIds (tag 'E' = 0x45)
                        let tag = (vid.0 >> 56) as u8;
                        if tag == 0x45 {
                            // Runtime error: kind = low bits (0=divZero, 1=overflow)
                            let kind = vid.0 & 0xFF;
                            let err_fn = pipeline.module.declare_function(
                                "runtime_error",
                                Linkage::Import,
                                &{
                                    let mut sig = Signature::new(pipeline.isa.default_call_conv());
                                    sig.params.push(AbiParam::new(types::I64)); // kind
                                    sig.returns.push(AbiParam::new(types::I64));
                                    sig
                                },
                            ).map_err(|e| EmitError::CraneliftError(e.to_string()))?;
                            let err_ref = pipeline.module.declare_func_in_func(err_fn, builder.func);
                            let kind_val = builder.ins().iconst(types::I64, kind as i64);
                            let inst = builder.ins().call(err_ref, &[kind_val]);
                            let result = builder.inst_results(inst)[0];
                            builder.declare_value_needs_stack_map(result);
                            return Ok(SsaVal::HeapPtr(result));
                        }

                        // Unresolved external — emit a call to unresolved_var_trap
                        eprintln!("[codegen] WARNING: unresolved var {:?} in {} (lambda_counter={})", vid, self.prefix, self.lambda_counter);
                        let trap_fn = pipeline.module.declare_function(
                            "unresolved_var_trap",
                            Linkage::Import,
                            &{
                                let mut sig = Signature::new(pipeline.isa.default_call_conv());
                                sig.params.push(AbiParam::new(types::I64)); // var_id
                                sig.returns.push(AbiParam::new(types::I64));
                                sig
                            },
                        ).map_err(|e| EmitError::CraneliftError(e.to_string()))?;
                        let trap_ref = pipeline.module.declare_func_in_func(trap_fn, builder.func);
                        let var_id_val = builder.ins().iconst(types::I64, vid.0 as i64);
                        let inst = builder.ins().call(trap_ref, &[var_id_val]);
                        let result = builder.inst_results(inst)[0];
                        builder.declare_value_needs_stack_map(result);
                        Ok(SsaVal::HeapPtr(result))
                    }
                };
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
                break Ok(SsaVal::HeapPtr(ptr));
            }
            CoreFrame::PrimOp { op, args } => {
                let mut arg_vals = Vec::new();
                for &a_idx in args {
                    arg_vals.push(self.emit_node(pipeline, builder, vmctx, gc_sig, tree, a_idx)?);
                }
                break primop::emit_primop(builder, op, &arg_vals);
            }
            CoreFrame::App { fun, arg } => {
                self.declare_env(builder);
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
                break Ok(SsaVal::HeapPtr(ret_val));
            }
            CoreFrame::Lam { binder, body } => {
                let body_tree = tree.extract_subtree(*body);
                let mut fvs = tidepool_repr::free_vars::free_vars(&body_tree);
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
                pipeline.register_lambda(lambda_func_id, lambda_name.clone());

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
                break Ok(SsaVal::HeapPtr(closure_ptr));
            }
            CoreFrame::LetNonRec { binder, rhs, body } => {
                // Dead code elimination: skip RHS if binder is unused in body.
                let body_fvs = tidepool_repr::free_vars::free_vars(&tree.extract_subtree(*body));
                // Debug: log DCE decisions for known problematic vars
                let known_bad = [8214565720323787988u64, 8214565720323787989, 8214565720323787990, 8214565720323784990, 3458764513820540932];
                if known_bad.contains(&binder.0) {
                    eprintln!("[dce] {:?} in_fvs={}", binder, body_fvs.contains(binder));
                }
                if body_fvs.contains(binder) {
                    let rhs_val = self.emit_node(pipeline, builder, vmctx, gc_sig, tree, *rhs)?;
                    self.env.insert(*binder, rhs_val);
                    let_cleanup.push(LetCleanup::Single(*binder));
                } else {
                    // Binder unused — skip RHS entirely
                }
                idx = *body;
                continue;
            }
            CoreFrame::LetRec { bindings, body } => {
                // Split bindings: Lam/Con need 3-phase pre-allocation (recursive),
                // everything else is evaluated eagerly as simple bindings first.
                let (rec_bindings, simple_bindings): (Vec<_>, Vec<_>) = bindings.iter().partition(|(_, rhs_idx)| {
                    matches!(&tree.nodes[*rhs_idx], CoreFrame::Lam { .. } | CoreFrame::Con { .. })
                });

                // Evaluate simple (non-recursive) bindings first
                for (binder, rhs_idx) in &simple_bindings {
                    let rhs_val = self.emit_node(pipeline, builder, vmctx, gc_sig, tree, *rhs_idx)?;
                    self.env.insert(*binder, rhs_val);
                }

                // If no recursive bindings remain, just emit body
                if rec_bindings.is_empty() {
                    let_cleanup.push(LetCleanup::Rec(bindings.iter().map(|(b, _)| *b).collect()));
                    idx = *body;
                    continue;
                }

                // Phase 1: Pre-allocate all recursive bindings (Lam and Con)
                enum PreAlloc {
                    Lam { binder: VarId, ptr: cranelift_codegen::ir::Value, fvs: Vec<VarId>, rhs_idx: usize },
                    Con { binder: VarId, ptr: cranelift_codegen::ir::Value, field_indices: Vec<usize> },
                }
                let mut pre_allocs = Vec::new();

                for (binder, rhs_idx) in &rec_bindings {
                    match &tree.nodes[*rhs_idx] {
                        CoreFrame::Lam { binder: lam_binder, body: lam_body } => {
                            let lam_body_tree = tree.extract_subtree(*lam_body);
                            let mut fvs = tidepool_repr::free_vars::free_vars(&lam_body_tree);
                            fvs.remove(lam_binder);
                            let mut sorted_fvs: Vec<VarId> = fvs.into_iter().filter(|v| {
                                self.env.contains_key(v) || rec_bindings.iter().any(|(b, _)| b == v)
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
                            pre_allocs.push(PreAlloc::Lam { binder: *binder, ptr: closure_ptr, fvs: sorted_fvs, rhs_idx: *rhs_idx });
                        }
                        CoreFrame::Con { tag, fields } => {
                            let num_fields = fields.len();
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

                            builder.declare_value_needs_stack_map(ptr);
                            pre_allocs.push(PreAlloc::Con { binder: *binder, ptr, field_indices: fields.clone() });
                        }
                        _ => unreachable!(),
                    }
                }

                // Phase 2: Bind all to their pre-allocated pointers
                for pa in &pre_allocs {
                    let (binder, ptr) = match pa {
                        PreAlloc::Lam { binder, ptr, .. } => (*binder, *ptr),
                        PreAlloc::Con { binder, ptr, .. } => (*binder, *ptr),
                    };
                    self.env.insert(binder, SsaVal::HeapPtr(ptr));
                }

                // Phase 3a: Fill Con fields (now that all bindings are in env)
                for pa in &pre_allocs {
                    if let PreAlloc::Con { ptr, field_indices, .. } = pa {
                        for (i, &f_idx) in field_indices.iter().enumerate() {
                            let val = self.emit_node(pipeline, builder, vmctx, gc_sig, tree, f_idx)?;
                            let field_val = ensure_heap_ptr(builder, vmctx, gc_sig, val);
                            builder.ins().store(MemFlags::trusted(), field_val, *ptr, CON_FIELDS_START + 8 * i as i32);
                        }
                    }
                }

                // Phase 3b: Compile Lam bodies and fill closures
                for pa in pre_allocs {
                    let (closure_ptr, sorted_fvs, rhs_idx) = match pa {
                        PreAlloc::Lam { ptr, fvs, rhs_idx, .. } => (ptr, fvs, rhs_idx),
                        PreAlloc::Con { .. } => continue,
                    };
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
                    pipeline.register_lambda(lambda_func_id, lambda_name.clone());

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

                let_cleanup.push(LetCleanup::Rec(bindings.iter().map(|(b, _)| *b).collect()));
                idx = *body;
                continue;
            }
            CoreFrame::Case { scrutinee, binder, alts } => {
                break crate::emit::case::emit_case(self, pipeline, builder, vmctx, gc_sig, tree, *scrutinee, binder, alts);
            }
            CoreFrame::Join { label, params, rhs, body } => {
                break crate::emit::join::emit_join(self, pipeline, builder, vmctx, gc_sig, tree, label, params, *rhs, *body);
            }
            CoreFrame::Jump { label, args } => {
                break crate::emit::join::emit_jump(self, pipeline, builder, vmctx, gc_sig, tree, label, args);
            }
        }
        }; // end loop
        // Clean up let-bindings in reverse order
        for cleanup in let_cleanup.into_iter().rev() {
            match cleanup {
                LetCleanup::Single(var) => { self.env.remove(&var); }
                LetCleanup::Rec(vars) => { for var in vars { self.env.remove(&var); } }
            }
        }
        result
    }
}

enum LetCleanup {
    Single(VarId),
    Rec(Vec<VarId>),
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

/// Emit a LitString as a heap Lit object pointing to a JIT data section.
///
/// Data section layout: [len: u64][bytes...]
/// Heap object layout: TAG_LIT at [0], size at [1..3], LIT_TAG_STRING at [8], data_ptr at [16]
fn emit_lit_string(
    pipeline: &mut CodegenPipeline,
    builder: &mut FunctionBuilder,
    vmctx: Value,
    gc_sig: ir::SigRef,
    bytes: &[u8],
    counter: &mut u32,
) -> Result<SsaVal, EmitError> {
    // Create data object: [len: u64][bytes...]
    let data_name = format!("__litstr_{}", *counter);
    *counter += 1;

    let data_id = pipeline.module
        .declare_data(&data_name, Linkage::Local, false, false)
        .map_err(|e| EmitError::CraneliftError(e.to_string()))?;

    let mut data_desc = DataDescription::new();
    data_desc.set_align(8); // Ensure 8-byte alignment for u64 length prefix
    let mut contents = Vec::with_capacity(8 + bytes.len());
    contents.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    contents.extend_from_slice(bytes);
    data_desc.define(contents.into_boxed_slice());

    pipeline.module
        .define_data(data_id, &data_desc)
        .map_err(|e| EmitError::CraneliftError(e.to_string()))?;

    // Get function-local reference to the data
    let local_data = pipeline.module.declare_data_in_func(data_id, builder.func);
    let data_ptr = builder.ins().symbol_value(types::I64, local_data);

    // Allocate 24-byte Lit heap object
    let ptr = emit_alloc_fast_path(builder, vmctx, LIT_TOTAL_SIZE, gc_sig);

    let tag = builder.ins().iconst(types::I8, layout::TAG_LIT as i64);
    builder.ins().store(MemFlags::trusted(), tag, ptr, 0);
    let size = builder.ins().iconst(types::I16, LIT_TOTAL_SIZE as i64);
    builder.ins().store(MemFlags::trusted(), size, ptr, 1);
    let lit_tag = builder.ins().iconst(types::I8, LIT_TAG_STRING);
    builder.ins().store(MemFlags::trusted(), lit_tag, ptr, LIT_TAG_OFFSET);
    builder.ins().store(MemFlags::trusted(), data_ptr, ptr, LIT_VALUE_OFFSET);

    builder.declare_value_needs_stack_map(ptr);
    Ok(SsaVal::HeapPtr(ptr))
}

pub(crate) fn ensure_heap_ptr(
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