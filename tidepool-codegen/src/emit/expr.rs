use crate::alloc::emit_alloc_fast_path;
use crate::emit::*;
use crate::pipeline::CodegenPipeline;
use cranelift_codegen::ir::{
    self, condcodes::IntCC, types, AbiParam, BlockArg, InstBuilder, MemFlags, Signature,
    UserFuncName, Value,
};
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{DataDescription, FuncId, Linkage, Module};
use recursion::{try_expand_and_collapse, MappableFrame};
use tidepool_heap::layout;
use tidepool_repr::*;

// ---------------------------------------------------------------------------
// EmitFrame: hylomorphism frame for stack-safe Cranelift IR emission
// ---------------------------------------------------------------------------

/// Uninhabited token type for MappableFrame impl.
enum EmitFrameToken {}

/// Classification of a Con field for the hylomorphism.
/// Eager fields are stack-safe hylo children; Deferred fields are compiled as thunks.
#[derive(Debug, Clone)]
enum ConField<A> {
    /// Processed by the hylomorphism's explicit stack (Var, Lit, Con, Lam, PrimOp, Jump, Join).
    Eager(A),
    /// Raw tree index, compiled as a thunk in the collapse phase (App, Case, LetNonRec, LetRec).
    Deferred(usize),
}

/// A single emission frame. `A` positions are children processed stack-safely
/// by the hylomorphism's internal explicit stack. Raw `usize` positions require
/// top-down context setup (block creation, pattern binding) and are processed
/// via bounded recursive calls in the collapse phase.
enum EmitFrame<A> {
    // Leaf nodes
    Var(VarId),
    Lit(Literal),
    LitString(Vec<u8>),

    // Simple recursive — children are A (stack-safe)
    Con {
        tag: DataConId,
        fields: Vec<ConField<A>>,
    },
    App {
        fun: A,
        arg: A,
    },
    PrimOp {
        op: PrimOpKind,
        args: Vec<A>,
    },
    Jump {
        label: JoinId,
        args: Vec<A>,
    },

    // Case: scrutinee is A (stack-safe), alt bodies are raw usize
    Case {
        scrutinee: A,
        binder: VarId,
        alts: Vec<Alt<usize>>,
    },

    // Lam: body compiled in a NEW function context in collapse
    Lam {
        binder: VarId,
        body_idx: usize,
    },

    // Join: body and rhs need block setup before emission
    Join {
        label: JoinId,
        params: Vec<VarId>,
        rhs_idx: usize,
        body_idx: usize,
    },

    // Let: delegate to emit_node's iterative loop
    LetBoundary(usize),
}

impl MappableFrame for EmitFrameToken {
    type Frame<X> = EmitFrame<X>;

    fn map_frame<A, B>(input: EmitFrame<A>, mut f: impl FnMut(A) -> B) -> EmitFrame<B> {
        match input {
            EmitFrame::Var(v) => EmitFrame::Var(v),
            EmitFrame::Lit(l) => EmitFrame::Lit(l),
            EmitFrame::LitString(b) => EmitFrame::LitString(b),
            EmitFrame::Con { tag, fields } => EmitFrame::Con {
                tag,
                fields: fields
                    .into_iter()
                    .map(|cf| match cf {
                        ConField::Eager(a) => ConField::Eager(f(a)),
                        ConField::Deferred(idx) => ConField::Deferred(idx),
                    })
                    .collect(),
            },
            EmitFrame::App { fun, arg } => EmitFrame::App {
                fun: f(fun),
                arg: f(arg),
            },
            EmitFrame::PrimOp { op, args } => EmitFrame::PrimOp {
                op,
                args: args.into_iter().map(&mut f).collect(),
            },
            EmitFrame::Jump { label, args } => EmitFrame::Jump {
                label,
                args: args.into_iter().map(&mut f).collect(),
            },
            EmitFrame::Case {
                scrutinee,
                binder,
                alts,
            } => EmitFrame::Case {
                scrutinee: f(scrutinee),
                binder,
                alts,
            },
            EmitFrame::Lam { binder, body_idx } => EmitFrame::Lam { binder, body_idx },
            EmitFrame::Join {
                label,
                params,
                rhs_idx,
                body_idx,
            } => EmitFrame::Join {
                label,
                params,
                rhs_idx,
                body_idx,
            },
            EmitFrame::LetBoundary(idx) => EmitFrame::LetBoundary(idx),
        }
    }
}

// ---------------------------------------------------------------------------
// Hylomorphism: expand + collapse
// ---------------------------------------------------------------------------

/// Expand: classify a tree node into an EmitFrame.
fn expand_node(tree: &CoreExpr, idx: usize) -> Result<EmitFrame<usize>, EmitError> {
    match &tree.nodes[idx] {
        CoreFrame::Var(v) => Ok(EmitFrame::Var(*v)),
        CoreFrame::Lit(Literal::LitString(bytes)) => Ok(EmitFrame::LitString(bytes.clone())),
        CoreFrame::Lit(lit) => Ok(EmitFrame::Lit(lit.clone())),
        CoreFrame::Con { tag, fields } => Ok(EmitFrame::Con {
            tag: *tag,
            fields: fields
                .iter()
                .map(|&f| {
                    if should_thunkify_con_field(tree, f) {
                        ConField::Deferred(f)
                    } else {
                        ConField::Eager(f)
                    }
                })
                .collect(),
        }),
        CoreFrame::App { fun, arg } => Ok(EmitFrame::App {
            fun: *fun,
            arg: *arg,
        }),
        CoreFrame::PrimOp { op, args } => Ok(EmitFrame::PrimOp {
            op: *op,
            args: args.clone(),
        }),
        CoreFrame::Jump { label, args } => Ok(EmitFrame::Jump {
            label: *label,
            args: args.clone(),
        }),
        CoreFrame::Case {
            scrutinee,
            binder,
            alts,
        } => Ok(EmitFrame::Case {
            scrutinee: *scrutinee,
            binder: *binder,
            alts: alts.clone(),
        }),
        CoreFrame::Lam { binder, body } => Ok(EmitFrame::Lam {
            binder: *binder,
            body_idx: *body,
        }),
        CoreFrame::Join {
            label,
            params,
            rhs,
            body,
        } => Ok(EmitFrame::Join {
            label: *label,
            params: params.clone(),
            rhs_idx: *rhs,
            body_idx: *body,
        }),
        CoreFrame::LetNonRec { .. } | CoreFrame::LetRec { .. } => Ok(EmitFrame::LetBoundary(idx)),
    }
}

/// Collapse: assemble Cranelift IR from child results.
#[allow(clippy::too_many_arguments)]
fn collapse_frame(
    ctx: &mut EmitContext,
    pipeline: &mut CodegenPipeline,
    builder: &mut FunctionBuilder,
    vmctx: Value,
    gc_sig: ir::SigRef,
    oom_func: ir::FuncRef,
    tree: &CoreExpr,
    frame: EmitFrame<SsaVal>,
) -> Result<SsaVal, EmitError> {
    match frame {
        EmitFrame::LitString(ref bytes) => emit_lit_string(
            pipeline,
            builder,
            vmctx,
            gc_sig,
            oom_func,
            bytes,
            &mut ctx.lambda_counter,
        ),
        EmitFrame::Lit(ref lit) => emit_lit(builder, vmctx, gc_sig, oom_func, lit),
        EmitFrame::Var(vid) => match ctx.env.get(&vid).copied() {
            Some(v) => Ok(v),
            None => {
                let tag = (vid.0 >> 56) as u8;
                if tag == 0x45 {
                    // Lazy poison: emit a constant pointer to a pre-allocated
                    // poison closure. The error flag is NOT set now — only when
                    // the closure is actually called (forced). This is critical
                    // for typeclass dictionaries that contain error methods for
                    // impossible branches (e.g., $fFloatingDouble).
                    let kind = vid.0 & 0xFF;
                    let poison_addr = crate::host_fns::error_poison_ptr_lazy(kind) as i64;
                    let poison_val = builder.ins().iconst(types::I64, poison_addr);
                    return Ok(SsaVal::HeapPtr(poison_val));
                }

                ctx.trace_scope(&format!(
                    "MISS var {:?} (env has {} entries)",
                    vid,
                    ctx.env.len()
                ));
                let trap_fn = pipeline
                    .module
                    .declare_function("unresolved_var_trap", Linkage::Import, &{
                        let mut sig = Signature::new(pipeline.isa.default_call_conv());
                        sig.params.push(AbiParam::new(types::I64));
                        sig.returns.push(AbiParam::new(types::I64));
                        sig
                    })
                    .map_err(|e| EmitError::CraneliftError(e.to_string()))?;
                let trap_ref = pipeline.module.declare_func_in_func(trap_fn, builder.func);
                let var_id_val = builder.ins().iconst(types::I64, vid.0 as i64);
                let inst = builder.ins().call(trap_ref, &[var_id_val]);
                let result = builder.inst_results(inst)[0];
                builder.declare_value_needs_stack_map(result);
                Ok(SsaVal::HeapPtr(result))
            }
        },
        EmitFrame::Con { tag, fields } => {
            let num_fields = fields.len();
            let mut field_vals: Vec<Value> = Vec::with_capacity(num_fields);

            for cf in &fields {
                match cf {
                    ConField::Eager(val) => {
                        field_vals.push(ensure_heap_ptr(builder, vmctx, gc_sig, oom_func, *val));
                    }
                    ConField::Deferred(idx) => {
                        let thunk_val = emit_thunk(
                            ctx, pipeline, builder, vmctx, gc_sig, oom_func, tree, *idx,
                        )?;
                        field_vals.push(thunk_val);
                    }
                }
            }

            let size = 24 + 8 * num_fields as u64;
            let ptr = emit_alloc_fast_path(builder, vmctx, size, gc_sig, oom_func);

            let tag_val = builder.ins().iconst(types::I8, layout::TAG_CON as i64);
            builder.ins().store(MemFlags::trusted(), tag_val, ptr, 0);
            let size_val = builder.ins().iconst(types::I16, size as i64);
            builder.ins().store(MemFlags::trusted(), size_val, ptr, 1);

            let con_tag_val = builder.ins().iconst(types::I64, tag.0 as i64);
            builder
                .ins()
                .store(MemFlags::trusted(), con_tag_val, ptr, CON_TAG_OFFSET);
            let num_fields_val = builder.ins().iconst(types::I16, num_fields as i64);
            builder.ins().store(
                MemFlags::trusted(),
                num_fields_val,
                ptr,
                CON_NUM_FIELDS_OFFSET,
            );

            for (i, field_val) in field_vals.into_iter().enumerate() {
                builder.ins().store(
                    MemFlags::trusted(),
                    field_val,
                    ptr,
                    CON_FIELDS_START + 8 * i as i32,
                );
            }

            builder.declare_value_needs_stack_map(ptr);
            Ok(SsaVal::HeapPtr(ptr))
        }
        EmitFrame::PrimOp { ref op, ref args } => {
            if matches!(op, tidepool_repr::PrimOpKind::Raise) {
                // raise# is GHC's exception primitive — used for impossible branches
                // and `error` calls. Emit a call to runtime_error(2) which sets a
                // thread-local error flag and returns null. The JIT machine converts
                // null results to Result::Err(JitError::Yield(UserError)).
                let err_fn = pipeline
                    .module
                    .declare_function("runtime_error", Linkage::Import, &{
                        let mut sig = Signature::new(pipeline.isa.default_call_conv());
                        sig.params.push(AbiParam::new(types::I64));
                        sig.returns.push(AbiParam::new(types::I64));
                        sig
                    })
                    .map_err(|e| EmitError::CraneliftError(e.to_string()))?;
                let err_ref = pipeline.module.declare_func_in_func(err_fn, builder.func);
                let kind_val = builder.ins().iconst(types::I64, 2); // UserError
                let inst = builder.ins().call(err_ref, &[kind_val]);
                let result = builder.inst_results(inst)[0];
                builder.declare_value_needs_stack_map(result);
                return Ok(SsaVal::HeapPtr(result));
            }
            primop::emit_primop(pipeline, builder, vmctx, gc_sig, oom_func, op, args)
        }
        EmitFrame::App { fun, arg } => {
            ctx.declare_env(builder);
            let fun_ptr = fun.value();
            let arg_ptr = ensure_heap_ptr(builder, vmctx, gc_sig, oom_func, arg);

            // Debug: call host fn to validate fun_ptr tag before call_indirect.
            // Returns 0 (null) if ok, or a poison pointer if call should be skipped.
            let check_fn = pipeline
                .module
                .declare_function("debug_app_check", Linkage::Import, &{
                    let mut sig = Signature::new(pipeline.isa.default_call_conv());
                    sig.params.push(AbiParam::new(types::I64)); // fun_ptr
                    sig.returns.push(AbiParam::new(types::I64)); // 0 = ok, non-zero = poison
                    sig
                })
                .map_err(|e| EmitError::CraneliftError(e.to_string()))?;
            let check_ref = pipeline.module.declare_func_in_func(check_fn, builder.func);
            let check_inst = builder.ins().call(check_ref, &[fun_ptr]);
            let check_result = builder.inst_results(check_inst)[0];

            // If debug_app_check returned non-zero (poison), short-circuit
            let call_block = builder.create_block();
            let merge_block = builder.create_block();
            builder.append_block_param(merge_block, types::I64);

            let is_zero = builder.ins().icmp_imm(IntCC::Equal, check_result, 0);
            builder.ins().brif(is_zero, call_block, &[], merge_block, &[BlockArg::Value(check_result)]);

            // call_block: normal function call
            builder.switch_to_block(call_block);
            builder.seal_block(call_block);

            let code_ptr = builder.ins().load(
                types::I64,
                MemFlags::trusted(),
                fun_ptr,
                CLOSURE_CODE_PTR_OFFSET,
            );

            let mut sig = Signature::new(pipeline.isa.default_call_conv());
            sig.params.push(AbiParam::new(types::I64)); // vmctx
            sig.params.push(AbiParam::new(types::I64)); // self
            sig.params.push(AbiParam::new(types::I64)); // arg
            sig.returns.push(AbiParam::new(types::I64));
            let call_sig = builder.import_signature(sig);

            let inst = builder
                .ins()
                .call_indirect(call_sig, code_ptr, &[vmctx, fun_ptr, arg_ptr]);
            let ret_val = builder.inst_results(inst)[0];
            builder.ins().jump(merge_block, &[BlockArg::Value(ret_val)]);

            // merge_block: result is either poison or call result
            builder.switch_to_block(merge_block);
            builder.seal_block(merge_block);
            let merged_val = builder.block_params(merge_block)[0];
            builder.declare_value_needs_stack_map(merged_val);
            Ok(SsaVal::HeapPtr(merged_val))
        }
        EmitFrame::Lam { binder, body_idx } => emit_lam(
            ctx, pipeline, builder, vmctx, gc_sig, oom_func, tree, binder, body_idx,
        ),
        EmitFrame::Case {
            scrutinee,
            binder,
            alts,
        } => crate::emit::case::emit_case(
            ctx, pipeline, builder, vmctx, gc_sig, oom_func, tree, scrutinee, &binder, &alts,
        ),
        EmitFrame::Join {
            label,
            params,
            rhs_idx,
            body_idx,
        } => crate::emit::join::emit_join(
            ctx, pipeline, builder, vmctx, gc_sig, oom_func, tree, &label, &params, rhs_idx,
            body_idx,
        ),
        EmitFrame::Jump { label, args } => {
            let join_block = ctx
                .join_blocks
                .get(&label)
                .ok_or_else(|| {
                    EmitError::NotYetImplemented(format!("Jump to unknown label {:?}", label))
                })?
                .block;

            let arg_values: Vec<BlockArg> = args
                .iter()
                .map(|v| BlockArg::Value(ensure_heap_ptr(builder, vmctx, gc_sig, oom_func, *v)))
                .collect();

            builder.ins().jump(join_block, &arg_values);

            let unreachable_block = builder.create_block();
            builder.switch_to_block(unreachable_block);
            builder.seal_block(unreachable_block);

            Ok(SsaVal::Raw(
                builder.ins().iconst(types::I64, 0),
                LIT_TAG_INT,
            ))
        }
        EmitFrame::LetBoundary(idx) => {
            ctx.emit_node(pipeline, builder, vmctx, gc_sig, oom_func, tree, idx)
        }
    }
}

/// Stack-safe emission of a non-Let expression subtree via hylomorphism.
#[allow(clippy::too_many_arguments)]
fn emit_subtree(
    ctx: &mut EmitContext,
    pipeline: &mut CodegenPipeline,
    builder: &mut FunctionBuilder,
    vmctx: Value,
    gc_sig: ir::SigRef,
    oom_func: ir::FuncRef,
    tree: &CoreExpr,
    idx: usize,
) -> Result<SsaVal, EmitError> {
    try_expand_and_collapse::<EmitFrameToken, _, _, _>(
        idx,
        |idx| expand_node(tree, idx),
        |frame| collapse_frame(ctx, pipeline, builder, vmctx, gc_sig, oom_func, tree, frame),
    )
}

/// Determine if a Con field should be compiled as a thunk (deferred evaluation).
/// Only genuinely computational expressions that might diverge or contain function calls
/// are thunked. Data construction (Con, Lit, Var, Lam, PrimOp) stays eager and is
/// processed by the hylomorphism's stack-safe explicit stack.
fn should_thunkify_con_field(tree: &CoreExpr, idx: usize) -> bool {
    matches!(
        &tree.nodes[idx],
        CoreFrame::App { .. }
            | CoreFrame::Case { .. }
            | CoreFrame::LetNonRec { .. }
            | CoreFrame::LetRec { .. }
    )
}

// ---------------------------------------------------------------------------
// Lam compilation helper (extracted for readability)
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn emit_lam(
    ctx: &mut EmitContext,
    pipeline: &mut CodegenPipeline,
    builder: &mut FunctionBuilder,
    vmctx: Value,
    gc_sig: ir::SigRef,
    oom_func: ir::FuncRef,
    tree: &CoreExpr,
    binder: VarId,
    body_idx: usize,
) -> Result<SsaVal, EmitError> {
    let body_tree = tree.extract_subtree(body_idx);
    let mut fvs = tidepool_repr::free_vars::free_vars(&body_tree);
    fvs.remove(&binder);

    let dropped: Vec<VarId> = fvs
        .iter()
        .filter(|v| !ctx.env.contains_key(v))
        .copied()
        .collect();
    if !dropped.is_empty() {
        ctx.trace_scope(&format!(
            "lam capture: dropped {} free vars not in scope: {:?}",
            dropped.len(),
            dropped
        ));
    }
    let mut sorted_fvs: Vec<VarId> = fvs
        .into_iter()
        .filter(|v| ctx.env.contains_key(v))
        .collect();
    sorted_fvs.sort_by_key(|v| v.0);

    let captures: Vec<(VarId, SsaVal)> = sorted_fvs
        .iter()
        .map(|v| {
            let val = ctx.env.get(v).ok_or_else(|| {
                EmitError::MissingCaptureVar(
                    *v,
                    format!("Lam capture: not in env (env has {} vars)", ctx.env.len()),
                )
            })?;
            Ok::<_, EmitError>((*v, *val))
        })
        .collect::<Result<Vec<_>, EmitError>>()?;

    let lambda_name = ctx.next_lambda_name();
    let mut closure_sig = Signature::new(pipeline.isa.default_call_conv());
    closure_sig.params.push(AbiParam::new(types::I64)); // vmctx
    closure_sig.params.push(AbiParam::new(types::I64)); // self
    closure_sig.params.push(AbiParam::new(types::I64)); // arg
    closure_sig.returns.push(AbiParam::new(types::I64));

    let lambda_func_id = pipeline
        .module
        .declare_function(&lambda_name, Linkage::Local, &closure_sig)
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

    let inner_oom_func = {
        let mut sig = Signature::new(pipeline.isa.default_call_conv());
        sig.returns.push(AbiParam::new(types::I64));
        let func_id = pipeline
            .module
            .declare_function("runtime_oom", Linkage::Import, &sig)
            .map_err(|e| EmitError::CraneliftError(format!("declare runtime_oom: {e}")))?;
        pipeline
            .module
            .declare_func_in_func(func_id, inner_builder.func)
    };

    let mut inner_emit = EmitContext::new(ctx.prefix.clone());
    inner_emit.lambda_counter = ctx.lambda_counter;

    inner_emit.trace_scope(&format!("insert lam binder {:?}", binder));
    inner_emit.env.insert(binder, SsaVal::HeapPtr(arg_param));

    for (i, (var_id, _)) in captures.iter().enumerate() {
        let offset = CLOSURE_CAPTURED_START + 8 * i as i32;
        let val = inner_builder
            .ins()
            .load(types::I64, MemFlags::trusted(), closure_self, offset);
        inner_builder.declare_value_needs_stack_map(val);
        inner_emit.trace_scope(&format!("insert lam capture {:?}", var_id));
        inner_emit.env.insert(*var_id, SsaVal::HeapPtr(val));
    }

    let body_root = body_tree.nodes.len() - 1;
    let body_result = inner_emit.emit_node(
        pipeline,
        &mut inner_builder,
        inner_vmctx,
        inner_gc_sig_ref,
        inner_oom_func,
        &body_tree,
        body_root,
    )?;
    let ret_val = ensure_heap_ptr(
        &mut inner_builder,
        inner_vmctx,
        inner_gc_sig_ref,
        inner_oom_func,
        body_result,
    );

    inner_builder.ins().return_(&[ret_val]);
    inner_builder.finalize();

    ctx.lambda_counter = inner_emit.lambda_counter;

    // Debug: dump Cranelift IR for each lambda when TIDEPOOL_DUMP_CLIF=1
    if std::env::var("TIDEPOOL_DUMP_CLIF").is_ok() {
        eprintln!("=== CLIF {} ({} captures) ===", lambda_name, captures.len());
        for (i, (var_id, ssaval)) in captures.iter().enumerate() {
            let kind = match ssaval {
                SsaVal::HeapPtr(_) => "HeapPtr",
                SsaVal::Raw(_, tag) => &format!("Raw(tag={})", tag),
            };
            eprintln!("  capture[{}]: VarId({:#x}) = {}", i, var_id.0, kind);
        }
        eprintln!("{}", inner_ctx.func.display());
        eprintln!("=== END CLIF {} ===", lambda_name);
    }

    pipeline.define_function(lambda_func_id, &mut inner_ctx)?;

    let func_ref = pipeline
        .module
        .declare_func_in_func(lambda_func_id, builder.func);
    let code_ptr = builder.ins().func_addr(types::I64, func_ref);

    let num_captures = captures.len();
    let closure_size = 24 + 8 * num_captures as u64;
    let closure_ptr = emit_alloc_fast_path(builder, vmctx, closure_size, gc_sig, oom_func);

    let tag_val = builder.ins().iconst(types::I8, layout::TAG_CLOSURE as i64);
    builder
        .ins()
        .store(MemFlags::trusted(), tag_val, closure_ptr, 0);
    let size_val = builder.ins().iconst(types::I16, closure_size as i64);
    builder
        .ins()
        .store(MemFlags::trusted(), size_val, closure_ptr, 1);

    builder.ins().store(
        MemFlags::trusted(),
        code_ptr,
        closure_ptr,
        CLOSURE_CODE_PTR_OFFSET,
    );
    let num_cap_val = builder.ins().iconst(types::I16, num_captures as i64);
    builder.ins().store(
        MemFlags::trusted(),
        num_cap_val,
        closure_ptr,
        CLOSURE_NUM_CAPTURED_OFFSET,
    );

    for (i, (_, ssaval)) in captures.iter().enumerate() {
        let cap_val = ensure_heap_ptr(builder, vmctx, gc_sig, oom_func, *ssaval);
        let offset = CLOSURE_CAPTURED_START + 8 * i as i32;
        builder
            .ins()
            .store(MemFlags::trusted(), cap_val, closure_ptr, offset);
    }

    builder.declare_value_needs_stack_map(closure_ptr);
    Ok(SsaVal::HeapPtr(closure_ptr))
}

/// Compile a non-trivial Con field as a thunk — a zero-arg closure that
/// evaluates the expression on first force. Mirrors `emit_lam` but with
/// a 2-param calling convention (vmctx, thunk_ptr) → result.
#[allow(clippy::too_many_arguments)]
fn emit_thunk(
    ctx: &mut EmitContext,
    pipeline: &mut CodegenPipeline,
    builder: &mut FunctionBuilder,
    vmctx: Value,
    gc_sig: ir::SigRef,
    oom_func: ir::FuncRef,
    tree: &CoreExpr,
    body_idx: usize,
) -> Result<Value, EmitError> {
    // 1. Extract subtree and find free variables
    let body_tree = tree.extract_subtree(body_idx);
    let fvs = tidepool_repr::free_vars::free_vars(&body_tree);

    let dropped: Vec<VarId> = fvs
        .iter()
        .filter(|v| !ctx.env.contains_key(v))
        .copied()
        .collect();
    if !dropped.is_empty() {
        ctx.trace_scope(&format!(
            "thunk capture: dropped {} free vars not in scope: {:?}",
            dropped.len(),
            dropped
        ));
    }
    let mut sorted_fvs: Vec<VarId> = fvs
        .into_iter()
        .filter(|v| ctx.env.contains_key(v))
        .collect();
    sorted_fvs.sort_by_key(|v| v.0);

    let captures: Vec<(VarId, SsaVal)> = sorted_fvs
        .iter()
        .map(|v| {
            let val = ctx.env.get(v).ok_or_else(|| {
                EmitError::MissingCaptureVar(
                    *v,
                    format!("Thunk capture: not in env (env has {} vars)", ctx.env.len()),
                )
            })?;
            Ok::<_, EmitError>((*v, *val))
        })
        .collect::<Result<Vec<_>, EmitError>>()?;

    // 2. Create thunk entry function: fn(vmctx, thunk_ptr) -> whnf_ptr
    let thunk_name = ctx.next_lambda_name(); // reuse lambda naming
    let mut thunk_sig = Signature::new(pipeline.isa.default_call_conv());
    thunk_sig.params.push(AbiParam::new(types::I64)); // vmctx
    thunk_sig.params.push(AbiParam::new(types::I64)); // thunk_ptr (self, for captures)
    thunk_sig.returns.push(AbiParam::new(types::I64)); // result

    let thunk_func_id = pipeline
        .module
        .declare_function(&thunk_name, Linkage::Local, &thunk_sig)
        .map_err(|e| EmitError::CraneliftError(e.to_string()))?;
    pipeline.register_lambda(thunk_func_id, thunk_name.clone());

    // 3. Build inner function
    let mut inner_ctx = Context::new();
    inner_ctx.func.signature = thunk_sig;
    inner_ctx.func.name = UserFuncName::default();

    let mut inner_fb_ctx = FunctionBuilderContext::new();
    let mut inner_builder = FunctionBuilder::new(&mut inner_ctx.func, &mut inner_fb_ctx);
    let inner_block = inner_builder.create_block();
    inner_builder.append_block_params_for_function_params(inner_block);
    inner_builder.switch_to_block(inner_block);
    inner_builder.seal_block(inner_block);

    let inner_vmctx = inner_builder.block_params(inner_block)[0];
    let thunk_self = inner_builder.block_params(inner_block)[1];
    inner_builder.declare_value_needs_stack_map(thunk_self);

    // GC sig + oom func for inner function
    let mut inner_gc_sig = Signature::new(pipeline.isa.default_call_conv());
    inner_gc_sig.params.push(AbiParam::new(types::I64));
    let inner_gc_sig_ref = inner_builder.import_signature(inner_gc_sig);

    let inner_oom_func = {
        let mut sig = Signature::new(pipeline.isa.default_call_conv());
        sig.returns.push(AbiParam::new(types::I64));
        let func_id = pipeline
            .module
            .declare_function("runtime_oom", Linkage::Import, &sig)
            .map_err(|e| EmitError::CraneliftError(format!("declare runtime_oom: {e}")))?;
        pipeline
            .module
            .declare_func_in_func(func_id, inner_builder.func)
    };

    // 4. Set up inner emit context, load captures from thunk_self
    let mut inner_emit = EmitContext::new(ctx.prefix.clone());
    inner_emit.lambda_counter = ctx.lambda_counter;

    for (i, (var_id, _)) in captures.iter().enumerate() {
        let offset = THUNK_CAPTURED_START + 8 * i as i32;
        let val = inner_builder
            .ins()
            .load(types::I64, MemFlags::trusted(), thunk_self, offset);
        inner_builder.declare_value_needs_stack_map(val);
        inner_emit.trace_scope(&format!("insert thunk capture {:?}", var_id));
        inner_emit.env.insert(*var_id, SsaVal::HeapPtr(val));
    }

    // 5. Compile thunked expression body
    let body_root = body_tree.nodes.len() - 1;
    let body_result = inner_emit.emit_node(
        pipeline,
        &mut inner_builder,
        inner_vmctx,
        inner_gc_sig_ref,
        inner_oom_func,
        &body_tree,
        body_root,
    )?;
    let ret_val = ensure_heap_ptr(
        &mut inner_builder,
        inner_vmctx,
        inner_gc_sig_ref,
        inner_oom_func,
        body_result,
    );

    inner_builder.ins().return_(&[ret_val]);
    inner_builder.finalize();

    ctx.lambda_counter = inner_emit.lambda_counter;

    // Debug: dump Cranelift IR when TIDEPOOL_DUMP_CLIF=1
    if std::env::var("TIDEPOOL_DUMP_CLIF").is_ok() {
        eprintln!("=== CLIF THUNK {} ({} captures) ===", thunk_name, captures.len());
        for (i, (var_id, ssaval)) in captures.iter().enumerate() {
            let kind = match ssaval {
                SsaVal::HeapPtr(_) => "HeapPtr",
                SsaVal::Raw(_, tag) => &format!("Raw(tag={})", tag),
            };
            eprintln!("  capture[{}]: VarId({:#x}) = {}", i, var_id.0, kind);
        }
        eprintln!("{}", inner_ctx.func.display());
        eprintln!("=== END CLIF THUNK {} ===", thunk_name);
    }

    pipeline.define_function(thunk_func_id, &mut inner_ctx)?;

    // 6. Allocate thunk heap object in outer function
    let func_ref = pipeline
        .module
        .declare_func_in_func(thunk_func_id, builder.func);
    let code_ptr = builder.ins().func_addr(types::I64, func_ref);

    let num_captures = captures.len();
    let thunk_size = 24 + 8 * num_captures as u64; // header(8) + state(8) + code_ptr(8) + captures
    let thunk_ptr = emit_alloc_fast_path(builder, vmctx, thunk_size, gc_sig, oom_func);

    // Write header: tag = TAG_THUNK, size
    let tag_val = builder.ins().iconst(types::I8, layout::TAG_THUNK as i64);
    builder
        .ins()
        .store(MemFlags::trusted(), tag_val, thunk_ptr, 0);
    let size_val = builder.ins().iconst(types::I16, thunk_size as i64);
    builder
        .ins()
        .store(MemFlags::trusted(), size_val, thunk_ptr, 1);

    // Write state = Unevaluated (0) at offset 8
    let state_val = builder.ins().iconst(types::I8, layout::THUNK_UNEVALUATED as i64);
    builder
        .ins()
        .store(MemFlags::trusted(), state_val, thunk_ptr, THUNK_STATE_OFFSET);

    // Write code pointer at offset 16
    builder
        .ins()
        .store(MemFlags::trusted(), code_ptr, thunk_ptr, THUNK_CODE_PTR_OFFSET);

    // Write captures at offset 24+
    for (i, (_, ssaval)) in captures.iter().enumerate() {
        let cap_val = ensure_heap_ptr(builder, vmctx, gc_sig, oom_func, *ssaval);
        let offset = THUNK_CAPTURED_START + 8 * i as i32;
        builder
            .ins()
            .store(MemFlags::trusted(), cap_val, thunk_ptr, offset);
    }

    builder.declare_value_needs_stack_map(thunk_ptr);
    Ok(thunk_ptr) // Return as Value (Cranelift), not SsaVal — caller wraps in Con
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Compile a CoreExpr into a JIT function. Returns the FuncId.
/// The compiled function has signature: (vmctx: i64) -> i64
/// It returns a heap pointer to the result.
pub fn compile_expr(
    pipeline: &mut CodegenPipeline,
    tree: &CoreExpr,
    name: &str,
) -> Result<FuncId, EmitError> {
    if std::env::var("TIDEPOOL_DUMP_TREE").is_ok() {
        eprintln!(
            "[tree] {} nodes:\n{}",
            tree.nodes.len(),
            tidepool_repr::pretty::pretty_print(tree)
        );
        let fvs = tidepool_repr::free_vars::free_vars(tree);
        if !fvs.is_empty() {
            eprintln!(
                "[tree] WARNING: {} free vars in input: {:?}",
                fvs.len(),
                fvs
            );
        }
    }

    let sig = pipeline.make_func_signature();
    let func_id = pipeline.declare_function(name)?;

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

    let oom_func = {
        let mut sig = Signature::new(pipeline.isa.default_call_conv());
        sig.returns.push(AbiParam::new(types::I64));
        let func_id = pipeline
            .module
            .declare_function("runtime_oom", Linkage::Import, &sig)
            .map_err(|e| EmitError::CraneliftError(format!("declare runtime_oom: {e}")))?;
        pipeline.module.declare_func_in_func(func_id, builder.func)
    };

    let mut emit_ctx = EmitContext::new(name.to_string());

    let result = emit_ctx.emit_node(
        pipeline,
        &mut builder,
        vmctx,
        gc_sig_ref,
        oom_func,
        tree,
        tree.nodes.len() - 1,
    )?;
    let ret = ensure_heap_ptr(&mut builder, vmctx, gc_sig_ref, oom_func, result);

    builder.ins().return_(&[ret]);
    builder.finalize();

    pipeline.define_function(func_id, &mut ctx)?;

    Ok(func_id)
}

impl EmitContext {
    /// Check if a binding's RHS references an error sentinel (tag 0x45).
    /// GHC Core hoists `error "..."` into let bindings that are only forced on
    /// impossible branches. Since our JIT is strict, we must not evaluate these
    /// eagerly. Returns true if the RHS free vars contain an error sentinel.
    /// Check if the RHS is a direct error call: either a bare error Var,
    /// or an App chain whose head function is an error Var.
    /// This is more precise than the old free-vars check, which would
    /// poison any binding that CONTAINED an error reference anywhere
    /// (e.g., in a case branch fallback), even if the main path was valid.
    fn rhs_is_error_call(tree: &CoreExpr, rhs_idx: usize) -> bool {
        let mut idx = rhs_idx;
        loop {
            match &tree.nodes[idx] {
                CoreFrame::Var(v) => return (v.0 >> 56) as u8 == 0x45,
                CoreFrame::App { fun, .. } => idx = *fun,
                _ => return false,
            }
        }
    }

    /// Extract the error kind from an error call (walks App chain to find head Var).
    fn extract_error_kind(tree: &CoreExpr, rhs_idx: usize) -> u64 {
        let mut idx = rhs_idx;
        loop {
            match &tree.nodes[idx] {
                CoreFrame::Var(v) if (v.0 >> 56) as u8 == 0x45 => return v.0 & 0xFF,
                CoreFrame::App { fun, .. } => idx = *fun,
                _ => return 2, // fallback: UserError
            }
        }
    }

    pub fn emit_node(
        &mut self,
        pipeline: &mut CodegenPipeline,
        builder: &mut FunctionBuilder,
        vmctx: Value,
        gc_sig: ir::SigRef,
        oom_func: ir::FuncRef,
        tree: &CoreExpr,
        mut idx: usize,
    ) -> Result<SsaVal, EmitError> {
        // Iterative tail-position loop: LetNonRec/LetRec body is in tail position,
        // so we iterate instead of recursing to avoid stack overflow on deep let-chains.
        let mut let_cleanup: Vec<LetCleanup> = Vec::new();
        let result = loop {
            match &tree.nodes[idx] {
                CoreFrame::LetNonRec { binder, rhs, body } => {
                    // Dead code elimination: skip RHS if binder is unused in body.
                    let body_fvs =
                        tidepool_repr::free_vars::free_vars(&tree.extract_subtree(*body));
                    if body_fvs.contains(binder) {
                        if Self::rhs_is_error_call(tree, *rhs) {
                            // Bind to lazy poison closure — error only triggers on call.
                            let kind = Self::extract_error_kind(tree, *rhs);
                            let poison_addr = crate::host_fns::error_poison_ptr_lazy(kind) as i64;
                            let poison_val = builder.ins().iconst(types::I64, poison_addr);
                            self.trace_scope(&format!("defer error LetNonRec {:?}", binder));
                            self.env.insert(*binder, SsaVal::HeapPtr(poison_val));
                        } else {
                            let rhs_val = self.emit_node(
                                pipeline, builder, vmctx, gc_sig, oom_func, tree, *rhs,
                            )?;
                            self.trace_scope(&format!("insert LetNonRec {:?}", binder));
                            self.env.insert(*binder, rhs_val);
                        }
                        let_cleanup.push(LetCleanup::Single(*binder));
                    } else {
                        self.trace_scope(&format!("DCE skip LetNonRec {:?}", binder));
                    }
                    idx = *body;
                    continue;
                }
                CoreFrame::LetRec { bindings, body } => {
                    // Split bindings: Lam/Con need 3-phase pre-allocation (recursive),
                    // everything else is evaluated eagerly as simple bindings first.
                    let (rec_bindings, simple_bindings): (Vec<_>, Vec<_>) =
                        bindings.iter().partition(|(_, rhs_idx)| {
                            matches!(
                                &tree.nodes[*rhs_idx],
                                CoreFrame::Lam { .. } | CoreFrame::Con { .. }
                            )
                        });
                    // If no recursive bindings, evaluate all as simple
                    if rec_bindings.is_empty() {
                        for (binder, rhs_idx) in &simple_bindings {
                            if Self::rhs_is_error_call(tree, *rhs_idx) {
                                let kind = Self::extract_error_kind(tree, *rhs_idx);
                                let poison_addr =
                                    crate::host_fns::error_poison_ptr_lazy(kind) as i64;
                                let poison_val = builder.ins().iconst(types::I64, poison_addr);
                                self.trace_scope(&format!(
                                    "defer error LetRec(simple) {:?}",
                                    binder
                                ));
                                self.env.insert(*binder, SsaVal::HeapPtr(poison_val));
                            } else {
                                let rhs_val = self.emit_node(
                                    pipeline, builder, vmctx, gc_sig, oom_func, tree, *rhs_idx,
                                )?;
                                self.trace_scope(&format!("insert LetRec(simple) {:?}", binder));
                                self.env.insert(*binder, rhs_val);
                            }
                        }
                        let_cleanup
                            .push(LetCleanup::Rec(bindings.iter().map(|(b, _)| *b).collect()));
                        idx = *body;
                        continue;
                    }

                    // Phase 1: Pre-allocate all recursive bindings (Lam and Con)
                    enum PreAlloc {
                        Lam {
                            binder: VarId,
                            ptr: cranelift_codegen::ir::Value,
                            fvs: Vec<VarId>,
                            rhs_idx: usize,
                        },
                        Con {
                            binder: VarId,
                            ptr: cranelift_codegen::ir::Value,
                            field_indices: Vec<usize>,
                        },
                    }
                    let mut pre_allocs = Vec::with_capacity(rec_bindings.len());

                    for (binder, rhs_idx) in &rec_bindings {
                        match &tree.nodes[*rhs_idx] {
                            CoreFrame::Lam {
                                binder: lam_binder,
                                body: lam_body,
                            } => {
                                let lam_body_tree = tree.extract_subtree(*lam_body);
                                let mut fvs = tidepool_repr::free_vars::free_vars(&lam_body_tree);
                                fvs.remove(lam_binder);
                                let dropped_fvs: Vec<VarId> = fvs
                                    .iter()
                                    .filter(|v| {
                                        !self.env.contains_key(v)
                                            && !rec_bindings.iter().any(|(b, _)| b == *v)
                                            && !simple_bindings.iter().any(|(b, _)| b == *v)
                                    })
                                    .copied()
                                    .collect();
                                if !dropped_fvs.is_empty() {
                                    self.trace_scope(&format!(
                                        "LetRec lam {:?}: dropped FVs {:?}",
                                        binder, dropped_fvs
                                    ));
                                }
                                let mut sorted_fvs: Vec<VarId> = fvs
                                    .into_iter()
                                    .filter(|v| {
                                        self.env.contains_key(v)
                                            || rec_bindings.iter().any(|(b, _)| b == v)
                                            || simple_bindings.iter().any(|(b, _)| b == v)
                                    })
                                    .collect();
                                sorted_fvs.sort_by_key(|v| v.0);

                                let num_captures = sorted_fvs.len();
                                let closure_size = 24 + 8 * num_captures as u64;
                                let closure_ptr = emit_alloc_fast_path(
                                    builder,
                                    vmctx,
                                    closure_size,
                                    gc_sig,
                                    oom_func,
                                );

                                let tag_val =
                                    builder.ins().iconst(types::I8, layout::TAG_CLOSURE as i64);
                                builder
                                    .ins()
                                    .store(MemFlags::trusted(), tag_val, closure_ptr, 0);
                                let size_val =
                                    builder.ins().iconst(types::I16, closure_size as i64);
                                builder
                                    .ins()
                                    .store(MemFlags::trusted(), size_val, closure_ptr, 1);
                                let num_cap_val =
                                    builder.ins().iconst(types::I16, num_captures as i64);
                                builder.ins().store(
                                    MemFlags::trusted(),
                                    num_cap_val,
                                    closure_ptr,
                                    CLOSURE_NUM_CAPTURED_OFFSET,
                                );

                                builder.declare_value_needs_stack_map(closure_ptr);
                                pre_allocs.push(PreAlloc::Lam {
                                    binder: *binder,
                                    ptr: closure_ptr,
                                    fvs: sorted_fvs,
                                    rhs_idx: *rhs_idx,
                                });
                            }
                            CoreFrame::Con { tag, fields } => {
                                let num_fields = fields.len();
                                let size = 24 + 8 * num_fields as u64;
                                let ptr =
                                    emit_alloc_fast_path(builder, vmctx, size, gc_sig, oom_func);

                                let tag_val =
                                    builder.ins().iconst(types::I8, layout::TAG_CON as i64);
                                builder.ins().store(MemFlags::trusted(), tag_val, ptr, 0);
                                let size_val = builder.ins().iconst(types::I16, size as i64);
                                builder.ins().store(MemFlags::trusted(), size_val, ptr, 1);
                                let con_tag_val = builder.ins().iconst(types::I64, tag.0 as i64);
                                builder.ins().store(
                                    MemFlags::trusted(),
                                    con_tag_val,
                                    ptr,
                                    CON_TAG_OFFSET,
                                );
                                let num_fields_val =
                                    builder.ins().iconst(types::I16, num_fields as i64);
                                builder.ins().store(
                                    MemFlags::trusted(),
                                    num_fields_val,
                                    ptr,
                                    CON_NUM_FIELDS_OFFSET,
                                );

                                // Zero-initialize Con fields so GC doesn't trace garbage
                                // if triggered before Phase 3b/3d.
                                let null_val = builder.ins().iconst(types::I64, 0);
                                for i in 0..num_fields {
                                    let offset = CON_FIELDS_START + 8 * i as i32;
                                    builder
                                        .ins()
                                        .store(MemFlags::trusted(), null_val, ptr, offset);
                                }

                                builder.declare_value_needs_stack_map(ptr);
                                pre_allocs.push(PreAlloc::Con {
                                    binder: *binder,
                                    ptr,
                                    field_indices: fields.clone(),
                                });
                            }
                            other => return Err(EmitError::InternalError(format!(
                                "LetRec phase 1: expected Lam or Con, got {:?}", other
                            ))),
                        }
                    }

                    // Phase 2: Bind all to their pre-allocated pointers
                    for pa in &pre_allocs {
                        let (binder, ptr) = match pa {
                            PreAlloc::Lam { binder, ptr, .. } => (*binder, *ptr),
                            PreAlloc::Con { binder, ptr, .. } => (*binder, *ptr),
                        };
                        self.trace_scope(&format!("insert LetRec(rec) {:?}", binder));
                        self.env.insert(binder, SsaVal::HeapPtr(ptr));
                    }

                    // Phase 2.5: Evaluate trivial simple bindings (Var aliases) before
                    // Lam body compilation. These are just env lookups that don't depend
                    // on closure code pointers. Resolved Lam bodies may capture them as
                    // free variables (e.g., substitute aliases like $fEqList_$s$c==1).
                    let mut deferred_simple = Vec::with_capacity(simple_bindings.len());
                    for (binder, rhs_idx) in &simple_bindings {
                        if Self::rhs_is_error_call(tree, *rhs_idx) {
                            let kind = Self::extract_error_kind(tree, *rhs_idx);
                            let poison_addr = crate::host_fns::error_poison_ptr_lazy(kind) as i64;
                            let poison_val = builder.ins().iconst(types::I64, poison_addr);
                            self.trace_scope(&format!("defer error LetRec(trivial) {:?}", binder));
                            self.env.insert(*binder, SsaVal::HeapPtr(poison_val));
                        } else if matches!(&tree.nodes[*rhs_idx], CoreFrame::Var(_)) {
                            let rhs_val = self.emit_node(
                                pipeline, builder, vmctx, gc_sig, oom_func, tree, *rhs_idx,
                            )?;
                            self.trace_scope(&format!("insert LetRec(trivial) {:?}", binder));
                            self.env.insert(*binder, rhs_val);
                        } else {
                            deferred_simple.push((*binder, *rhs_idx));
                        }
                    }

                    // Phase 3a: Compile Lam bodies and set code pointers.
                    // Capture VALUES are NOT filled here — some captures reference
                    // deferred simple bindings (Phase 3c) that aren't in env yet.
                    // We compile the inner function (which reads captures by slot
                    // position) and store code pointers, then fill capture slots
                    // in Phase 3a' after simple bindings are evaluated.
                    let mut pending_capture_updates: std::collections::HashMap<
                        VarId,
                        Vec<(cranelift_codegen::ir::Value, i32)>,
                    > = std::collections::HashMap::with_capacity(rec_bindings.len());

                    for pa in &pre_allocs {
                        let (closure_ptr, sorted_fvs, rhs_idx) = match pa {
                            PreAlloc::Lam {
                                ptr, fvs, rhs_idx, ..
                            } => (*ptr, fvs, *rhs_idx),
                            PreAlloc::Con { .. } => continue,
                        };
                        let (lam_binder, lam_body) = match &tree.nodes[rhs_idx] {
                            CoreFrame::Lam { binder, body } => (*binder, *body),
                            other => return Err(EmitError::InternalError(format!(
                                "LetRec phase 3a: expected Lam, got {:?}", other
                            ))),
                        };
                        let lam_body_tree = tree.extract_subtree(lam_body);

                        let lambda_name = self.next_lambda_name();
                        let mut closure_sig = Signature::new(pipeline.isa.default_call_conv());
                        closure_sig.params.push(AbiParam::new(types::I64));
                        closure_sig.params.push(AbiParam::new(types::I64));
                        closure_sig.params.push(AbiParam::new(types::I64));
                        closure_sig.returns.push(AbiParam::new(types::I64));

                        let lambda_func_id = pipeline
                            .module
                            .declare_function(&lambda_name, Linkage::Local, &closure_sig)
                            .map_err(|e| EmitError::CraneliftError(e.to_string()))?;
                        pipeline.register_lambda(lambda_func_id, lambda_name.clone());

                        let mut inner_ctx = Context::new();
                        inner_ctx.func.signature = closure_sig;
                        inner_ctx.func.name = UserFuncName::default();

                        let mut inner_fb_ctx = FunctionBuilderContext::new();
                        let mut inner_builder =
                            FunctionBuilder::new(&mut inner_ctx.func, &mut inner_fb_ctx);
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

                        let inner_oom_func = {
                            let mut sig = Signature::new(pipeline.isa.default_call_conv());
                            sig.returns.push(AbiParam::new(types::I64));
                            let func_id = pipeline
                                .module
                                .declare_function("runtime_oom", Linkage::Import, &sig)
                                .map_err(|e| EmitError::CraneliftError(format!("declare runtime_oom: {e}")))?;
                            pipeline
                                .module
                                .declare_func_in_func(func_id, inner_builder.func)
                        };

                        let mut inner_emit = EmitContext::new(self.prefix.clone());
                        inner_emit.lambda_counter = self.lambda_counter;
                        inner_emit
                            .env
                            .insert(lam_binder, SsaVal::HeapPtr(inner_arg));

                        // Load captures by position — the inner function doesn't need
                        // the outer SSA values, just the slot offsets.
                        for (i, var_id) in sorted_fvs.iter().enumerate() {
                            let offset = CLOSURE_CAPTURED_START + 8 * i as i32;
                            let val = inner_builder.ins().load(
                                types::I64,
                                MemFlags::trusted(),
                                inner_self,
                                offset,
                            );
                            inner_builder.declare_value_needs_stack_map(val);
                            inner_emit.env.insert(*var_id, SsaVal::HeapPtr(val));
                        }

                        let body_root = lam_body_tree.nodes.len() - 1;
                        let body_result = inner_emit.emit_node(
                            pipeline,
                            &mut inner_builder,
                            inner_vmctx,
                            inner_gc_sig_ref,
                            inner_oom_func,
                            &lam_body_tree,
                            body_root,
                        )?;
                        let ret_val = ensure_heap_ptr(
                            &mut inner_builder,
                            inner_vmctx,
                            inner_gc_sig_ref,
                            inner_oom_func,
                            body_result,
                        );

                        inner_builder.ins().return_(&[ret_val]);
                        inner_builder.finalize();
                        self.lambda_counter = inner_emit.lambda_counter;
                        pipeline.define_function(lambda_func_id, &mut inner_ctx)?;

                        let func_ref = pipeline
                            .module
                            .declare_func_in_func(lambda_func_id, builder.func);
                        let code_ptr = builder.ins().func_addr(types::I64, func_ref);
                        builder.ins().store(
                            MemFlags::trusted(),
                            code_ptr,
                            closure_ptr,
                            CLOSURE_CODE_PTR_OFFSET,
                        );

                        // Zero-initialize capture slots so GC doesn't trace garbage
                        // if triggered before Phase 3a'.
                        let null_val = builder.ins().iconst(types::I64, 0);
                        for i in 0..sorted_fvs.len() {
                            let offset = CLOSURE_CAPTURED_START + 8 * i as i32;
                            builder
                                .ins()
                                .store(MemFlags::trusted(), null_val, closure_ptr, offset);
                        }

                        // Fill captures that are already in env. Defer those that
                        // reference deferred simple bindings (not yet evaluated).
                        for (i, var_id) in sorted_fvs.iter().enumerate() {
                            let offset = CLOSURE_CAPTURED_START + 8 * i as i32;
                            if let Some(ssaval) = self.env.get(var_id) {
                                let cap_val =
                                    ensure_heap_ptr(builder, vmctx, gc_sig, oom_func, *ssaval);
                                builder.ins().store(
                                    MemFlags::trusted(),
                                    cap_val,
                                    closure_ptr,
                                    offset,
                                );
                            } else {
                                pending_capture_updates
                                    .entry(*var_id)
                                    .or_default()
                                    .push((closure_ptr, offset));
                            }
                        }
                    }

                    // Phase 3b: Fill Con fields that DON'T reference deferred simple
                    // bindings. These are safe to fill now — at runtime, function calls
                    // in simple bindings may pattern-match on these Cons, so their fields
                    // must be populated before any simple binding evaluation.
                    let simple_binder_set: std::collections::HashSet<VarId> =
                        deferred_simple.iter().map(|(b, _)| *b).collect();
                    let mut deferred_cons: Vec<(cranelift_codegen::ir::Value, Vec<usize>)> =
                        Vec::with_capacity(rec_bindings.len());
                    for pa in &pre_allocs {
                        if let PreAlloc::Con {
                            ptr, field_indices, ..
                        } = pa
                        {
                            let needs_simple = field_indices.iter().any(|&f_idx| {
                            matches!(&tree.nodes[f_idx], CoreFrame::Var(v) if simple_binder_set.contains(v))
                        });
                            if needs_simple {
                                deferred_cons.push((*ptr, field_indices.clone()));
                            } else {
                                for (i, &f_idx) in field_indices.iter().enumerate() {
                                    let field_val = if should_thunkify_con_field(tree, f_idx) {
                                        emit_thunk(
                                            self, pipeline, builder, vmctx, gc_sig, oom_func, tree, f_idx,
                                        )?
                                    } else {
                                        let val = self.emit_node(
                                            pipeline, builder, vmctx, gc_sig, oom_func, tree, f_idx,
                                        )?;
                                        ensure_heap_ptr(builder, vmctx, gc_sig, oom_func, val)
                                    };
                                    builder.ins().store(
                                        MemFlags::trusted(),
                                        field_val,
                                        *ptr,
                                        CON_FIELDS_START + 8 * i as i32,
                                    );
                                }
                            }
                        }
                    }

                    // Phase 3c: Evaluate deferred simple bindings (now that Con fields
                    // they may access at runtime are populated).
                    // Topologically sort: if binding A references binding B in deferred_simple,
                    // B must be evaluated first. This includes dependencies mediated by closures.
                    let deferred_simple = {
                        let deferred_set: std::collections::HashSet<VarId> =
                            deferred_simple.iter().map(|(b, _)| *b).collect();

                        let mut direct_deps: std::collections::HashMap<VarId, Vec<VarId>> =
                            std::collections::HashMap::with_capacity(bindings.len());
                        for (binder, rhs_idx) in bindings {
                            let fvs = tidepool_repr::free_vars::free_vars(
                                &tree.extract_subtree(*rhs_idx),
                            );
                            direct_deps.insert(*binder, fvs.into_iter().collect());
                        }

                        // Compute reachability on-demand using DFS
                        let mut reachable_deferred: std::collections::HashMap<
                            VarId,
                            std::collections::HashSet<VarId>,
                        > = std::collections::HashMap::with_capacity(deferred_simple.len());
                        for &(start_node, _) in &deferred_simple {
                            let mut visited = std::collections::HashSet::new();
                            let mut stack = vec![start_node];
                            let mut reached = std::collections::HashSet::new();

                            while let Some(node) = stack.pop() {
                                if !visited.insert(node) {
                                    continue;
                                }
                                if node != start_node && deferred_set.contains(&node) {
                                    reached.insert(node);
                                }
                                if let Some(neighbors) = direct_deps.get(&node) {
                                    for &next in neighbors {
                                        stack.push(next);
                                    }
                                }
                            }
                            reachable_deferred.insert(start_node, reached);
                        }

                        let mut sorted = Vec::with_capacity(deferred_simple.len());
                        let mut remaining: Vec<(VarId, usize)> = deferred_simple;
                        let mut progress = true;
                        while !remaining.is_empty() && progress {
                            progress = false;
                            let mut next_remaining = Vec::with_capacity(remaining.len());
                            for (binder, rhs_idx) in remaining {
                                let blocked = reachable_deferred[&binder].iter().any(|fv| {
                                    !sorted.iter().any(|(b, _): &(VarId, usize)| *b == *fv)
                                });
                                if blocked {
                                    next_remaining.push((binder, rhs_idx));
                                } else {
                                    sorted.push((binder, rhs_idx));
                                    progress = true;
                                }
                            }
                            remaining = next_remaining;
                        }
                        // Any remaining (cyclic deps) — append as-is
                        sorted.extend(remaining);
                        sorted
                    };

                    // For each deferred Con, track which simple bindings it
                    // depends on.  Once all deps are satisfied we fill ALL its
                    // fields (not just the simple-binding ones).
                    let mut deferred_con_deps: Vec<(
                        cranelift_codegen::ir::Value,
                        Vec<usize>,
                        std::collections::HashSet<VarId>,
                    )> = Vec::with_capacity(deferred_cons.len());
                    for (ptr, field_indices) in &deferred_cons {
                        let deps: std::collections::HashSet<VarId> = field_indices
                            .iter()
                            .filter_map(|&f_idx| {
                                if let CoreFrame::Var(v) = &tree.nodes[f_idx] {
                                    if simple_binder_set.contains(v) {
                                        return Some(*v);
                                    }
                                }
                                None
                            })
                            .collect();
                        deferred_con_deps.push((*ptr, field_indices.clone(), deps));
                    }

                    for (binder, rhs_idx) in &deferred_simple {
                        if Self::rhs_is_error_call(tree, *rhs_idx) {
                            let kind = Self::extract_error_kind(tree, *rhs_idx);
                            let poison_addr = crate::host_fns::error_poison_ptr_lazy(kind) as i64;
                            let poison_val = builder.ins().iconst(types::I64, poison_addr);
                            self.trace_scope(&format!("defer error LetRec(deferred) {:?}", binder));
                            self.env.insert(*binder, SsaVal::HeapPtr(poison_val));
                        } else {
                            let rhs_val = self.emit_node(
                                pipeline, builder, vmctx, gc_sig, oom_func, tree, *rhs_idx,
                            )?;
                            self.trace_scope(&format!("insert LetRec(simple) {:?}", binder));
                            self.env.insert(*binder, rhs_val);
                        }

                        // Incrementally fill pending captures as dependencies become available!
                        // This guarantees that closures have their capture slots filled before
                        // subsequent simple bindings in this LetRec are evaluated, which might
                        // invoke those closures.
                        if let Some(updates) = pending_capture_updates.remove(binder) {
                            if let Some(ssaval) = self.env.get(binder) {
                                let cap_val =
                                    ensure_heap_ptr(builder, vmctx, gc_sig, oom_func, *ssaval);
                                for (closure_ptr, offset) in updates {
                                    builder.ins().store(
                                        MemFlags::trusted(),
                                        cap_val,
                                        closure_ptr,
                                        offset,
                                    );
                                }
                            }
                        }

                        // Incrementally fill deferred Cons whose simple-binding
                        // deps are now all satisfied.  Fill ALL fields at once so
                        // that later simple bindings (or their callees) can safely
                        // pattern-match on these Cons without hitting NULL fields.
                        for (ptr, field_indices, remaining_deps) in deferred_con_deps.iter_mut() {
                            remaining_deps.remove(binder);
                            if remaining_deps.is_empty() && !field_indices.is_empty() {
                                for (i, &f_idx) in field_indices.iter().enumerate() {
                                    let field_val = if should_thunkify_con_field(tree, f_idx) {
                                        emit_thunk(
                                            self, pipeline, builder, vmctx, gc_sig, oom_func, tree, f_idx,
                                        )?
                                    } else {
                                        let val = self.emit_node(
                                            pipeline, builder, vmctx, gc_sig, oom_func, tree, f_idx,
                                        )?;
                                        ensure_heap_ptr(builder, vmctx, gc_sig, oom_func, val)
                                    };
                                    builder.ins().store(
                                        MemFlags::trusted(),
                                        field_val,
                                        *ptr,
                                        CON_FIELDS_START + 8 * i as i32,
                                    );
                                }
                                // Mark as filled so Phase 3d skips it.
                                *field_indices = Vec::new();
                            }
                        }
                    }

                    // Phase 3a': Fill any remaining closure capture slots.
                    // These are captures of Lam/Con bindings (or trivial simple bindings)
                    // that were not in env during Phase 1 but are now in env.
                    for (var_id, updates) in pending_capture_updates {
                        let ssaval = self.env.get(&var_id).ok_or_else(|| {
                            EmitError::MissingCaptureVar(
                                var_id,
                                "LetRec Phase 3a' capture fill: not in env after Phase 3c".into(),
                            )
                        })?;
                        let cap_val = ensure_heap_ptr(builder, vmctx, gc_sig, oom_func, *ssaval);
                        for (closure_ptr, offset) in updates {
                            builder
                                .ins()
                                .store(MemFlags::trusted(), cap_val, closure_ptr, offset);
                        }
                    }

                    // Phase 3d: Fill any deferred Con fields not already filled
                    // incrementally during Phase 3c.
                    for (ptr, field_indices, _) in &deferred_con_deps {
                        for (i, &f_idx) in field_indices.iter().enumerate() {
                            let field_val = if should_thunkify_con_field(tree, f_idx) {
                                emit_thunk(
                                    self, pipeline, builder, vmctx, gc_sig, oom_func, tree, f_idx,
                                )?
                            } else {
                                let val = self.emit_node(
                                    pipeline, builder, vmctx, gc_sig, oom_func, tree, f_idx,
                                )?;
                                ensure_heap_ptr(builder, vmctx, gc_sig, oom_func, val)
                            };
                            builder.ins().store(
                                MemFlags::trusted(),
                                field_val,
                                *ptr,
                                CON_FIELDS_START + 8 * i as i32,
                            );
                        }
                    }

                    let_cleanup.push(LetCleanup::Rec(bindings.iter().map(|(b, _)| *b).collect()));
                    idx = *body;
                    continue;
                }
                // All non-Let nodes: delegate to stack-safe hylomorphism
                _ => {
                    break emit_subtree(
                        self, pipeline, builder, vmctx, gc_sig, oom_func, tree, idx,
                    );
                }
            }
        }; // end loop
           // Clean up let-bindings in reverse order
        for cleanup in let_cleanup.into_iter().rev() {
            match cleanup {
                LetCleanup::Single(var) => {
                    self.trace_scope(&format!("remove LetCleanup {:?}", var));
                    self.env.remove(&var);
                }
                LetCleanup::Rec(vars) => {
                    for var in &vars {
                        self.trace_scope(&format!("remove LetCleanup(rec) {:?}", var));
                    }
                    for var in vars {
                        self.env.remove(&var);
                    }
                }
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
    oom_func: ir::FuncRef,
    lit: &Literal,
) -> Result<SsaVal, EmitError> {
    let ptr = emit_alloc_fast_path(builder, vmctx, LIT_TOTAL_SIZE, gc_sig, oom_func);

    let tag = builder.ins().iconst(types::I8, layout::TAG_LIT as i64);
    builder.ins().store(MemFlags::trusted(), tag, ptr, 0);
    let size = builder.ins().iconst(types::I16, LIT_TOTAL_SIZE as i64);
    builder.ins().store(MemFlags::trusted(), size, ptr, 1);

    match lit {
        Literal::LitInt(n) => {
            let lit_tag = builder.ins().iconst(types::I8, LIT_TAG_INT);
            builder
                .ins()
                .store(MemFlags::trusted(), lit_tag, ptr, LIT_TAG_OFFSET);
            let val = builder.ins().iconst(types::I64, *n);
            builder
                .ins()
                .store(MemFlags::trusted(), val, ptr, LIT_VALUE_OFFSET);
            builder.declare_value_needs_stack_map(ptr);
            Ok(SsaVal::HeapPtr(ptr))
        }
        Literal::LitWord(n) => {
            let lit_tag = builder.ins().iconst(types::I8, LIT_TAG_WORD);
            builder
                .ins()
                .store(MemFlags::trusted(), lit_tag, ptr, LIT_TAG_OFFSET);
            let val = builder.ins().iconst(types::I64, *n as i64);
            builder
                .ins()
                .store(MemFlags::trusted(), val, ptr, LIT_VALUE_OFFSET);
            builder.declare_value_needs_stack_map(ptr);
            Ok(SsaVal::HeapPtr(ptr))
        }
        Literal::LitChar(c) => {
            let lit_tag = builder.ins().iconst(types::I8, LIT_TAG_CHAR);
            builder
                .ins()
                .store(MemFlags::trusted(), lit_tag, ptr, LIT_TAG_OFFSET);
            let val = builder.ins().iconst(types::I64, *c as i64);
            builder
                .ins()
                .store(MemFlags::trusted(), val, ptr, LIT_VALUE_OFFSET);
            builder.declare_value_needs_stack_map(ptr);
            Ok(SsaVal::HeapPtr(ptr))
        }
        Literal::LitFloat(bits) => {
            let lit_tag = builder.ins().iconst(types::I8, LIT_TAG_FLOAT);
            builder
                .ins()
                .store(MemFlags::trusted(), lit_tag, ptr, LIT_TAG_OFFSET);
            let val = builder.ins().iconst(types::I64, *bits as i64);
            builder
                .ins()
                .store(MemFlags::trusted(), val, ptr, LIT_VALUE_OFFSET);
            builder.declare_value_needs_stack_map(ptr);
            Ok(SsaVal::HeapPtr(ptr))
        }
        Literal::LitDouble(bits) => {
            let lit_tag = builder.ins().iconst(types::I8, LIT_TAG_DOUBLE);
            builder
                .ins()
                .store(MemFlags::trusted(), lit_tag, ptr, LIT_TAG_OFFSET);
            let val = builder.ins().iconst(types::I64, *bits as i64);
            builder
                .ins()
                .store(MemFlags::trusted(), val, ptr, LIT_VALUE_OFFSET);
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
    oom_func: ir::FuncRef,
    bytes: &[u8],
    counter: &mut u32,
) -> Result<SsaVal, EmitError> {
    // Create data object: [len: u64][bytes...]
    let data_name = format!("__litstr_{}", *counter);
    *counter += 1;

    let data_id = pipeline
        .module
        .declare_data(&data_name, Linkage::Local, false, false)
        .map_err(|e| EmitError::CraneliftError(e.to_string()))?;

    let mut data_desc = DataDescription::new();
    data_desc.set_align(8); // Ensure 8-byte alignment for u64 length prefix
    let mut contents = Vec::with_capacity(8 + bytes.len() + 1);
    contents.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    contents.extend_from_slice(bytes);
    contents.push(0); // Null terminator for GHC's Addr# string iteration
    data_desc.define(contents.into_boxed_slice());

    pipeline
        .module
        .define_data(data_id, &data_desc)
        .map_err(|e| EmitError::CraneliftError(e.to_string()))?;

    // Get function-local reference to the data
    let local_data = pipeline.module.declare_data_in_func(data_id, builder.func);
    let data_ptr = builder.ins().symbol_value(types::I64, local_data);

    // Allocate 24-byte Lit heap object
    let ptr = emit_alloc_fast_path(builder, vmctx, LIT_TOTAL_SIZE, gc_sig, oom_func);

    let tag = builder.ins().iconst(types::I8, layout::TAG_LIT as i64);
    builder.ins().store(MemFlags::trusted(), tag, ptr, 0);
    let size = builder.ins().iconst(types::I16, LIT_TOTAL_SIZE as i64);
    builder.ins().store(MemFlags::trusted(), size, ptr, 1);
    let lit_tag = builder.ins().iconst(types::I8, LIT_TAG_STRING);
    builder
        .ins()
        .store(MemFlags::trusted(), lit_tag, ptr, LIT_TAG_OFFSET);
    builder
        .ins()
        .store(MemFlags::trusted(), data_ptr, ptr, LIT_VALUE_OFFSET);

    builder.declare_value_needs_stack_map(ptr);
    Ok(SsaVal::HeapPtr(ptr))
}

pub(crate) fn ensure_heap_ptr(
    builder: &mut FunctionBuilder,
    vmctx: Value,
    gc_sig: ir::SigRef,
    oom_func: ir::FuncRef,
    val: SsaVal,
) -> Value {
    match val {
        SsaVal::HeapPtr(v) => v,
        SsaVal::Raw(v, lit_tag) => {
            let ptr = emit_alloc_fast_path(builder, vmctx, LIT_TOTAL_SIZE, gc_sig, oom_func);
            let tag = builder.ins().iconst(types::I8, layout::TAG_LIT as i64);
            builder.ins().store(MemFlags::trusted(), tag, ptr, 0);
            let size = builder.ins().iconst(types::I16, LIT_TOTAL_SIZE as i64);
            builder.ins().store(MemFlags::trusted(), size, ptr, 1);
            let lit_tag_val = builder.ins().iconst(types::I8, lit_tag);
            builder
                .ins()
                .store(MemFlags::trusted(), lit_tag_val, ptr, LIT_TAG_OFFSET);
            builder
                .ins()
                .store(MemFlags::trusted(), v, ptr, LIT_VALUE_OFFSET);
            builder.declare_value_needs_stack_map(ptr);
            ptr
        }
    }
}
