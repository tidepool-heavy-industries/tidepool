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
use rustc_hash::{FxHashMap, FxHashSet};
use tidepool_heap::layout;
use tidepool_repr::*;

// ---------------------------------------------------------------------------
// EmitFrame: hylomorphism frame for stack-safe Cranelift IR emission
// ---------------------------------------------------------------------------

/// Uninhabited token type for MappableFrame impl.
enum EmitFrameToken {}

/// A single emission frame. `A` positions are children processed stack-safely
/// by the hylomorphism's internal explicit stack. Raw `usize` positions require
/// top-down context setup (block creation, pattern binding) and are processed
/// via bounded recursive calls in the collapse phase.
enum EmitFrame<A> {
    // Leaf nodes
    Var(VarId),
    Lit(Literal),
    LitString(Vec<u8>),
    LitByteArray(Vec<u8>),

    // Simple recursive \u2014 children are A (stack-safe)
    Con {
        tag: DataConId,
        fields: Vec<A>,
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

    // Con with non-trivial fields: all field indices are raw usize,
    // handled in collapse by emitting thunks for non-trivial fields.
    ThunkCon {
        tag: DataConId,
        field_indices: Vec<usize>,
    },

    // Let: delegate to emit_node's iterative loop
    LetBoundary(usize),

    /// Direct error call (PrimOp Raise or sentinel Var App).
    Raise {
        kind: u64,
        msg: Option<Vec<u8>>,
        arg: Option<A>,
    },
}

impl MappableFrame for EmitFrameToken {
    type Frame<X> = EmitFrame<X>;

    fn map_frame<A, B>(input: EmitFrame<A>, mut f: impl FnMut(A) -> B) -> EmitFrame<B> {
        match input {
            EmitFrame::Var(v) => EmitFrame::Var(v),
            EmitFrame::Lit(l) => EmitFrame::Lit(l),
            EmitFrame::LitString(b) => EmitFrame::LitString(b),
            EmitFrame::LitByteArray(b) => EmitFrame::LitByteArray(b),
            EmitFrame::Con { tag, fields } => EmitFrame::Con {
                tag,
                fields: fields.into_iter().map(&mut f).collect(),
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
            EmitFrame::ThunkCon { tag, field_indices } => {
                EmitFrame::ThunkCon { tag, field_indices }
            }
            EmitFrame::LetBoundary(idx) => EmitFrame::LetBoundary(idx),
            EmitFrame::Raise { kind, msg, arg } => EmitFrame::Raise {
                kind,
                msg,
                arg: arg.map(f),
            },
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
        CoreFrame::Lit(Literal::LitByteArray(bytes)) => Ok(EmitFrame::LitByteArray(bytes.clone())),
        CoreFrame::Lit(lit) => Ok(EmitFrame::Lit(lit.clone())),
        CoreFrame::Con { tag, fields } => {
            let has_non_trivial = fields.iter().any(|&f| !is_trivial_field(f, tree));
            if has_non_trivial {
                Ok(EmitFrame::ThunkCon {
                    tag: *tag,
                    field_indices: fields.clone(),
                })
            } else {
                Ok(EmitFrame::Con {
                    tag: *tag,
                    fields: fields.clone(),
                })
            }
        }
        CoreFrame::App { fun, arg } => {
            if EmitContext::rhs_is_error_call(tree, idx) {
                if let Some(msg) = EmitContext::extract_error_message(tree, idx) {
                    // Static fast path: the message is known at compile time.
                    let kind = EmitContext::extract_error_kind(tree, idx);
                    return Ok(EmitFrame::Raise {
                        kind,
                        msg: Some(msg),
                        arg: None,
                    });
                }
                // No static message: compile as a NORMAL application. The
                // sentinel Var emits a lazy poison closure; applying it routes
                // through poison_trampoline_lazy, which swallows non-string
                // arguments (CallStack dicts in partial applications like
                // `error cs`) by returning itself and raises with the
                // materialized message once a string-ish argument arrives.
                // Eagerly raising here is wrong for partial applications.
            }
            Ok(EmitFrame::App {
                fun: *fun,
                arg: *arg,
            })
        }
        CoreFrame::PrimOp { op, args } => {
            if matches!(op, PrimOpKind::Raise) {
                let msg = if !args.is_empty() {
                    if let CoreFrame::Lit(Literal::LitString(bytes)) = &tree.nodes[args[0]] {
                        Some(bytes.clone())
                    } else {
                        None
                    }
                } else {
                    None
                };
                let arg = if !args.is_empty() {
                    Some(args[0])
                } else {
                    None
                };
                Ok(EmitFrame::Raise { kind: 2, msg, arg })
            } else {
                Ok(EmitFrame::PrimOp {
                    op: *op,
                    args: args.clone(),
                })
            }
        }
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
/// Emit-coverage key for an `EmitFrame` variant (see `crate::coverage`).
fn emit_frame_cov_key(frame: &EmitFrame<SsaVal>) -> &'static str {
    match frame {
        EmitFrame::Var(_) => "frame:Var",
        EmitFrame::Lit(_) => "frame:Lit",
        EmitFrame::LitString(_) => "frame:LitString",
        EmitFrame::LitByteArray(_) => "frame:LitByteArray",
        EmitFrame::Con { .. } => "frame:Con",
        EmitFrame::ThunkCon { .. } => "frame:ThunkCon",
        EmitFrame::App { .. } => "frame:App",
        EmitFrame::PrimOp { .. } => "frame:PrimOp",
        EmitFrame::Jump { .. } => "frame:Jump",
        EmitFrame::Case { .. } => "frame:Case",
        EmitFrame::Lam { .. } => "frame:Lam",
        EmitFrame::Join { .. } => "frame:Join",
        EmitFrame::LetBoundary(_) => "frame:LetBoundary",
        EmitFrame::Raise { .. } => "frame:Raise",
    }
}

fn collapse_frame(args: EmitArgs, frame: EmitFrame<SsaVal>) -> Result<SsaVal, EmitError> {
    let tail = args.tail;
    crate::coverage::hit(emit_frame_cov_key(&frame));
    match frame {
        EmitFrame::LitString(ref bytes) => emit_lit_string(
            args.sess.pipeline,
            args.builder,
            args.sess.vmctx,
            args.sess.gc_sig,
            args.sess.oom_func,
            bytes,
            &mut args.ctx.lambda_counter,
        ),
        EmitFrame::LitByteArray(ref bytes) => emit_lit_bytearray_literal(
            args.sess.pipeline,
            args.builder,
            args.sess.vmctx,
            args.sess.gc_sig,
            args.sess.oom_func,
            bytes,
            &mut args.ctx.lambda_counter,
        ),
        EmitFrame::Lit(ref lit) => emit_lit(
            args.builder,
            args.sess.vmctx,
            args.sess.gc_sig,
            args.sess.oom_func,
            lit,
        ),
        EmitFrame::Var(vid) => match args.ctx.env.get(&vid).copied() {
            Some(v) => Ok(v),
            None => {
                let tag = (vid.0 >> 56) as u8;
                if tag == tidepool_repr::ERROR_SENTINEL_TAG {
                    // Lazy poison: emit a constant pointer to a pre-allocated
                    // poison closure. The error flag is NOT set now \u2014 only when
                    // the closure is actually called (forced). This is critical
                    // for typeclass dictionaries that contain error methods for
                    // impossible branches (e.g., $fFloatingDouble).
                    let kind = vid.0 & 0xFF;
                    let poison_addr = crate::host_fns::error_poison_ptr_lazy(kind) as i64;
                    let poison_val = args.builder.ins().iconst(types::I64, poison_addr);
                    return Ok(SsaVal::HeapPtr(poison_val));
                }

                if crate::debug::trace_level() >= crate::debug::TraceLevel::Scope {
                    args.ctx.trace_scope(&format!(
                        "MISS var {:?} (env has {} entries)",
                        vid,
                        args.ctx.env.len()
                    ));
                }
                let trap_fn = args
                    .sess
                    .pipeline
                    .module
                    .declare_function("unresolved_var_trap", Linkage::Import, &{
                        let mut sig = Signature::new(args.sess.pipeline.isa.default_call_conv());
                        sig.params.push(AbiParam::new(types::I64));
                        sig.returns.push(AbiParam::new(types::I64));
                        sig
                    })
                    .map_err(|e| EmitError::CraneliftError(e.to_string()))?;
                let trap_ref = args
                    .sess
                    .pipeline
                    .module
                    .declare_func_in_func(trap_fn, args.builder.func);
                let var_id_val = args.builder.ins().iconst(types::I64, vid.0 as i64);
                let inst = args.builder.ins().call(trap_ref, &[var_id_val]);
                let result = args.builder.inst_results(inst)[0];
                args.builder.declare_value_needs_stack_map(result);
                Ok(SsaVal::HeapPtr(result))
            }
        },
        EmitFrame::Con { tag, fields } => {
            crate::coverage::hit(if fields.is_empty() {
                "con:nullary"
            } else {
                "con:nonnullary"
            });
            let field_vals: Vec<Value> = fields
                .iter()
                .map(|v| {
                    ensure_heap_ptr(
                        args.builder,
                        args.sess.vmctx,
                        args.sess.gc_sig,
                        args.sess.oom_func,
                        *v,
                    )
                })
                .collect();

            let num_fields = field_vals.len();
            let size = 24 + 8 * num_fields as u64;
            let ptr = emit_alloc_fast_path(
                args.builder,
                args.sess.vmctx,
                size,
                args.sess.gc_sig,
                args.sess.oom_func,
            );

            let tag_val = args.builder.ins().iconst(types::I8, layout::TAG_CON as i64);
            args.builder
                .ins()
                .store(MemFlags::trusted(), tag_val, ptr, 0);
            let size_val = args.builder.ins().iconst(types::I16, size as i64);
            args.builder
                .ins()
                .store(MemFlags::trusted(), size_val, ptr, 1);

            let con_tag_val = args.builder.ins().iconst(types::I64, tag.0 as i64);
            args.builder
                .ins()
                .store(MemFlags::trusted(), con_tag_val, ptr, CON_TAG_OFFSET);
            let num_fields_val = args.builder.ins().iconst(types::I16, num_fields as i64);
            args.builder.ins().store(
                MemFlags::trusted(),
                num_fields_val,
                ptr,
                CON_NUM_FIELDS_OFFSET,
            );

            for (i, field_val) in field_vals.into_iter().enumerate() {
                args.builder.ins().store(
                    MemFlags::trusted(),
                    field_val,
                    ptr,
                    CON_FIELDS_OFFSET + 8 * i as i32,
                );
            }

            args.builder.declare_value_needs_stack_map(ptr);
            Ok(SsaVal::HeapPtr(ptr))
        }
        EmitFrame::ThunkCon { tag, field_indices } => {
            // Con with non-trivial fields: evaluate trivial fields eagerly,
            // compile non-trivial fields as thunks.
            let num_fields = field_indices.len();
            let size = 24 + 8 * num_fields as u64;
            let ptr = emit_alloc_fast_path(
                args.builder,
                args.sess.vmctx,
                size,
                args.sess.gc_sig,
                args.sess.oom_func,
            );

            let tag_val = args.builder.ins().iconst(types::I8, layout::TAG_CON as i64);
            args.builder
                .ins()
                .store(MemFlags::trusted(), tag_val, ptr, 0);
            let size_val = args.builder.ins().iconst(types::I16, size as i64);
            args.builder
                .ins()
                .store(MemFlags::trusted(), size_val, ptr, 1);

            let con_tag_val = args.builder.ins().iconst(types::I64, tag.0 as i64);
            args.builder
                .ins()
                .store(MemFlags::trusted(), con_tag_val, ptr, CON_TAG_OFFSET);
            let num_fields_val = args.builder.ins().iconst(types::I16, num_fields as i64);
            args.builder.ins().store(
                MemFlags::trusted(),
                num_fields_val,
                ptr,
                CON_NUM_FIELDS_OFFSET,
            );

            for (i, &f_idx) in field_indices.iter().enumerate() {
                let field_val = if is_trivial_field(f_idx, args.sess.tree) {
                    // Trivial: evaluate eagerly (existing path)
                    let val = EmitContext::emit_node(
                        EmitArgs {
                            ctx: args.ctx,
                            sess: args.sess,
                            builder: args.builder,
                            tail: TailCtx::NonTail,
                        },
                        f_idx,
                    )?;
                    ensure_heap_ptr(
                        args.builder,
                        args.sess.vmctx,
                        args.sess.gc_sig,
                        args.sess.oom_func,
                        val,
                    )
                } else {
                    // Non-trivial: compile as thunk
                    let thunk_val = emit_thunk(
                        EmitArgs {
                            ctx: args.ctx,
                            sess: args.sess,
                            builder: args.builder,
                            tail: TailCtx::NonTail,
                        },
                        f_idx,
                    )?;
                    thunk_val.value()
                };
                args.builder.ins().store(
                    MemFlags::trusted(),
                    field_val,
                    ptr,
                    CON_FIELDS_OFFSET + 8 * i as i32,
                );
            }

            args.builder.declare_value_needs_stack_map(ptr);
            Ok(SsaVal::HeapPtr(ptr))
        }
        EmitFrame::PrimOp {
            ref op,
            args: ref prim_args,
        } => {
            crate::coverage::hit(op.serial_name()); // per-primop emit coverage
                                                    // Force thunked args: PrimOps are strict in all arguments.
                                                    // Case alt binders can be thunks (lazy Con fields), so force
                                                    // them before passing to primop unboxing.
            let forced_args: Vec<SsaVal> = prim_args
                .iter()
                .map(|a| force_thunk_ssaval(args.sess.pipeline, args.builder, args.sess.vmctx, *a))
                .collect::<Result<Vec<_>, EmitError>>()?;
            primop::emit_primop(args.sess, args.builder, op, &forced_args)
        }
        EmitFrame::App { fun, arg } => {
            args.ctx.declare_env(args.builder);
            let raw_fun_ptr = fun.value();
            let arg_ptr = ensure_heap_ptr(
                args.builder,
                args.sess.vmctx,
                args.sess.gc_sig,
                args.sess.oom_func,
                arg,
            );

            // Force thunked function values. Case alt binders can be
            // thunks (lazy fields), so when one is applied as a function,
            // we must force it to get the underlying closure.
            let fun_tag = args
                .builder
                .ins()
                .load(types::I8, MemFlags::trusted(), raw_fun_ptr, 0);
            let is_thunk = args.builder.ins().icmp_imm(
                IntCC::Equal,
                fun_tag,
                tidepool_heap::layout::TAG_THUNK as i64,
            );

            let force_fun_block = args.builder.create_block();
            let fun_ready_block = args.builder.create_block();
            args.builder.append_block_param(fun_ready_block, types::I64);

            args.builder.ins().brif(
                is_thunk,
                force_fun_block,
                &[],
                fun_ready_block,
                &[BlockArg::Value(raw_fun_ptr)],
            );

            args.builder.switch_to_block(force_fun_block);
            args.builder.seal_block(force_fun_block);

            let force_fn = args
                .sess
                .pipeline
                .module
                .declare_function("heap_force", Linkage::Import, &{
                    let mut sig = Signature::new(args.sess.pipeline.isa.default_call_conv());
                    sig.params.push(AbiParam::new(types::I64)); // vmctx
                    sig.params.push(AbiParam::new(types::I64)); // thunk
                    sig.returns.push(AbiParam::new(types::I64)); // result
                    sig
                })
                .map_err(|e| EmitError::CraneliftError(e.to_string()))?;
            let force_ref = args
                .sess
                .pipeline
                .module
                .declare_func_in_func(force_fn, args.builder.func);
            let force_call = args
                .builder
                .ins()
                .call(force_ref, &[args.sess.vmctx, raw_fun_ptr]);
            let forced_fun = args.builder.inst_results(force_call)[0];
            args.builder.declare_value_needs_stack_map(forced_fun);
            args.builder
                .ins()
                .jump(fun_ready_block, &[BlockArg::Value(forced_fun)]);

            args.builder.switch_to_block(fun_ready_block);
            args.builder.seal_block(fun_ready_block);
            let fun_ptr = args.builder.block_params(fun_ready_block)[0];
            args.builder.declare_value_needs_stack_map(fun_ptr);

            // Debug: call host fn to validate fun_ptr tag before call_indirect.
            // Returns 0 (null) if ok, or a poison pointer if call should be skipped.
            let check_fn = args
                .sess
                .pipeline
                .module
                .declare_function("debug_app_check", Linkage::Import, &{
                    let mut sig = Signature::new(args.sess.pipeline.isa.default_call_conv());
                    sig.params.push(AbiParam::new(types::I64)); // fun_ptr
                    sig.returns.push(AbiParam::new(types::I64)); // 0 = ok, non-zero = poison
                    sig
                })
                .map_err(|e| EmitError::CraneliftError(e.to_string()))?;
            let check_ref = args
                .sess
                .pipeline
                .module
                .declare_func_in_func(check_fn, args.builder.func);
            let check_inst = args.builder.ins().call(check_ref, &[fun_ptr]);
            let check_result = args.builder.inst_results(check_inst)[0];

            // If debug_app_check returned non-zero (poison), short-circuit
            let call_block = args.builder.create_block();
            let merge_block = args.builder.create_block();
            args.builder.append_block_param(merge_block, types::I64);

            let is_zero = args.builder.ins().icmp_imm(IntCC::Equal, check_result, 0);
            args.builder.ins().brif(
                is_zero,
                call_block,
                &[],
                merge_block,
                &[BlockArg::Value(check_result)],
            );

            // call_block: normal function call
            args.builder.switch_to_block(call_block);
            args.builder.seal_block(call_block);

            let code_ptr = args.builder.ins().load(
                types::I64,
                MemFlags::trusted(),
                fun_ptr,
                CLOSURE_CODE_PTR_OFFSET,
            );

            let mut sig = Signature::new(args.sess.pipeline.isa.default_call_conv());
            sig.params.push(AbiParam::new(types::I64)); // vmctx
            sig.params.push(AbiParam::new(types::I64)); // self
            sig.params.push(AbiParam::new(types::I64)); // arg
            sig.returns.push(AbiParam::new(types::I64));
            let call_sig = args.builder.import_signature(sig);

            let inst = args.builder.ins().call_indirect(
                call_sig,
                code_ptr,
                &[args.sess.vmctx, fun_ptr, arg_ptr],
            );
            let ret_val = args.builder.inst_results(inst)[0];

            // TCO null check: if callee returned null, it might be a tail call
            let ret_is_null = args.builder.ins().icmp_imm(IntCC::Equal, ret_val, 0);
            let null_check_block = args.builder.create_block();
            let ret_ok_block = args.builder.create_block();

            args.builder
                .ins()
                .brif(ret_is_null, null_check_block, &[], ret_ok_block, &[]);

            // ret_ok_block: normal return, jump to merge
            args.builder.switch_to_block(ret_ok_block);
            args.builder.seal_block(ret_ok_block);
            args.builder
                .ins()
                .jump(merge_block, &[BlockArg::Value(ret_val)]);

            // null_check_block: check if VMContext has a pending tail call
            args.builder.switch_to_block(null_check_block);
            args.builder.seal_block(null_check_block);

            let tail_callee = args.builder.ins().load(
                types::I64,
                MemFlags::trusted(),
                args.sess.vmctx,
                VMCTX_TAIL_CALLEE_OFFSET,
            );
            let has_tail_call = args.builder.ins().icmp_imm(IntCC::NotEqual, tail_callee, 0);

            let resolve_block = args.builder.create_block();
            let null_propagate_block = args.builder.create_block();

            args.builder
                .ins()
                .brif(has_tail_call, resolve_block, &[], null_propagate_block, &[]);

            // null_propagate_block: no tail call pending, propagate null (error)
            args.builder.switch_to_block(null_propagate_block);
            args.builder.seal_block(null_propagate_block);
            let null_val = args.builder.ins().iconst(types::I64, 0);
            args.builder
                .ins()
                .jump(merge_block, &[BlockArg::Value(null_val)]);

            // resolve_block: call trampoline_resolve to execute the pending tail call
            args.builder.switch_to_block(resolve_block);
            args.builder.seal_block(resolve_block);

            let resolve_fn = args
                .sess
                .pipeline
                .module
                .declare_function("trampoline_resolve", Linkage::Import, &{
                    let mut sig = Signature::new(args.sess.pipeline.isa.default_call_conv());
                    sig.params.push(AbiParam::new(types::I64)); // vmctx
                    sig.returns.push(AbiParam::new(types::I64)); // result
                    sig
                })
                .map_err(|e: cranelift_module::ModuleError| {
                    EmitError::CraneliftError(e.to_string())
                })?;
            let resolve_ref = args
                .sess
                .pipeline
                .module
                .declare_func_in_func(resolve_fn, args.builder.func);
            let resolve_inst = args.builder.ins().call(resolve_ref, &[args.sess.vmctx]);
            let resolved_val = args.builder.inst_results(resolve_inst)[0];
            args.builder.declare_value_needs_stack_map(resolved_val);
            args.builder
                .ins()
                .jump(merge_block, &[BlockArg::Value(resolved_val)]);

            // merge_block: result from any path
            args.builder.switch_to_block(merge_block);
            args.builder.seal_block(merge_block);
            let merged_val = args.builder.block_params(merge_block)[0];
            args.builder.declare_value_needs_stack_map(merged_val);
            Ok(SsaVal::HeapPtr(merged_val))
        }
        EmitFrame::Lam { binder, body_idx } => emit_lam(
            EmitArgs {
                ctx: args.ctx,
                sess: args.sess,
                builder: args.builder,
                tail: TailCtx::NonTail,
            },
            binder,
            body_idx,
        ),
        EmitFrame::Case {
            scrutinee,
            binder,
            alts,
        } => crate::emit::case::emit_case(
            EmitArgs {
                ctx: args.ctx,
                sess: args.sess,
                builder: args.builder,
                tail,
            },
            scrutinee,
            &binder,
            &alts,
        ),
        EmitFrame::Join {
            label,
            params,
            rhs_idx,
            body_idx,
        } => crate::emit::join::emit_join(
            EmitArgs {
                ctx: args.ctx,
                sess: args.sess,
                builder: args.builder,
                tail,
            },
            &label,
            &params,
            rhs_idx,
            body_idx,
        ),
        EmitFrame::Jump {
            label,
            args: jump_args,
        } => {
            let join_block = args.ctx.join_blocks.get(&label)?.block;

            let arg_values: Vec<BlockArg> = jump_args
                .iter()
                .map(|v| {
                    BlockArg::Value(ensure_heap_ptr(
                        args.builder,
                        args.sess.vmctx,
                        args.sess.gc_sig,
                        args.sess.oom_func,
                        *v,
                    ))
                })
                .collect();

            args.builder.ins().jump(join_block, &arg_values);

            let unreachable_block = args.builder.create_block();
            args.builder.switch_to_block(unreachable_block);
            args.builder.seal_block(unreachable_block);

            Ok(SsaVal::Raw(
                args.builder.ins().iconst(types::I64, 0),
                LIT_TAG_INT,
            ))
        }
        EmitFrame::LetBoundary(idx) => {
            // A LetBoundary appearing as a mapped child of a frame (e.g.,
            // Case scrutinee, App argument) is NEVER in tail position —
            // the parent frame still has work to do after this sub-expression.
            // Without this, a LetRec body App inside a Case scrutinee gets
            // compiled as a tail call, bypassing the Case dispatch entirely.
            EmitContext::emit_node(
                EmitArgs {
                    ctx: args.ctx,
                    sess: args.sess,
                    builder: args.builder,
                    tail: TailCtx::NonTail,
                },
                idx,
            )
        }
        EmitFrame::Raise { kind, msg, arg } => {
            if let Some(bytes) = msg {
                let msg_val = emit_lit_string(
                    args.sess.pipeline,
                    args.builder,
                    args.sess.vmctx,
                    args.sess.gc_sig,
                    args.sess.oom_func,
                    &bytes,
                    &mut args.ctx.lambda_counter,
                )?;
                let msg_ptr = msg_val.value();

                // Extract data_ptr and len from LitString heap object
                let raw_ptr = args.builder.ins().load(
                    types::I64,
                    MemFlags::trusted(),
                    msg_ptr,
                    LIT_VALUE_OFFSET,
                );
                let len = args
                    .builder
                    .ins()
                    .load(types::I64, MemFlags::trusted(), raw_ptr, 0);
                let bytes_ptr = args.builder.ins().iadd_imm(raw_ptr, 8);

                let err_fn = args
                    .sess
                    .pipeline
                    .module
                    .declare_function("runtime_error_with_msg", Linkage::Import, &{
                        let mut sig = Signature::new(args.sess.pipeline.isa.default_call_conv());
                        sig.params.push(AbiParam::new(types::I64)); // kind
                        sig.params.push(AbiParam::new(types::I64)); // msg_ptr
                        sig.params.push(AbiParam::new(types::I64)); // msg_len
                        sig.returns.push(AbiParam::new(types::I64));
                        sig
                    })
                    .map_err(|e| EmitError::CraneliftError(e.to_string()))?;
                let err_ref = args
                    .sess
                    .pipeline
                    .module
                    .declare_func_in_func(err_fn, args.builder.func);

                let kind_val = args.builder.ins().iconst(types::I64, kind as i64);
                let inst = args
                    .builder
                    .ins()
                    .call(err_ref, &[kind_val, bytes_ptr, len]);
                let result = args.builder.inst_results(inst)[0];
                args.builder.declare_value_needs_stack_map(result);
                Ok(SsaVal::HeapPtr(result))
            } else if let Some(arg_val) = arg {
                let arg_ptr = ensure_heap_ptr(
                    args.builder,
                    args.sess.vmctx,
                    args.sess.gc_sig,
                    args.sess.oom_func,
                    arg_val,
                );

                // Message not statically known (floated binding, thunk-subtree
                // capture, or dynamically built): materialize it at runtime
                // from the live argument in the host, which handles LitString,
                // Text, String cons-lists, thunk forcing, and fallback.
                let err_fn = args
                    .sess
                    .pipeline
                    .module
                    .declare_function("runtime_error_dynamic", Linkage::Import, &{
                        let mut sig = Signature::new(args.sess.pipeline.isa.default_call_conv());
                        sig.params.push(AbiParam::new(types::I64)); // vmctx
                        sig.params.push(AbiParam::new(types::I64)); // kind
                        sig.params.push(AbiParam::new(types::I64)); // arg
                        sig.returns.push(AbiParam::new(types::I64));
                        sig
                    })
                    .map_err(|e| EmitError::CraneliftError(e.to_string()))?;
                let err_ref = args
                    .sess
                    .pipeline
                    .module
                    .declare_func_in_func(err_fn, args.builder.func);
                let kind_val = args.builder.ins().iconst(types::I64, kind as i64);
                let inst = args
                    .builder
                    .ins()
                    .call(err_ref, &[args.sess.vmctx, kind_val, arg_ptr]);
                let result = args.builder.inst_results(inst)[0];
                args.builder.declare_value_needs_stack_map(result);
                Ok(SsaVal::HeapPtr(result))
            } else {
                let err_fn = args
                    .sess
                    .pipeline
                    .module
                    .declare_function("runtime_error", Linkage::Import, &{
                        let mut sig = Signature::new(args.sess.pipeline.isa.default_call_conv());
                        sig.params.push(AbiParam::new(types::I64));
                        sig.returns.push(AbiParam::new(types::I64));
                        sig
                    })
                    .map_err(|e| EmitError::CraneliftError(e.to_string()))?;
                let err_ref = args
                    .sess
                    .pipeline
                    .module
                    .declare_func_in_func(err_fn, args.builder.func);
                let kind_val = args.builder.ins().iconst(types::I64, kind as i64);
                let inst = args.builder.ins().call(err_ref, &[kind_val]);
                let result = args.builder.inst_results(inst)[0];
                args.builder.declare_value_needs_stack_map(result);
                Ok(SsaVal::HeapPtr(result))
            }
        }
    }
}

/// Stack-safe emission of a non-Let expression subtree via hylomorphism.
fn emit_subtree(mut args: EmitArgs, idx: usize) -> Result<SsaVal, EmitError> {
    args.tail = TailCtx::NonTail;
    emit_subtree_with_tail(args, idx)
}

/// Stack-safe emission of a value-position subtree via hylomorphism.
///
/// INVARIANT: the hylomorphism NEVER carries a `Tail` context. Every child the
/// recursion crate visits bottom-up (App fun/arg, PrimOp args, Case scrutinee,
/// Jump args, Con fields) is a *value position* — its result is consumed locally,
/// so a tail call there would `return null` and escape as the enclosing
/// function's result (see #313 t11). Tail-ness is owned exclusively by the
/// evaluation spine in `emit_node`, which dispatches tail App/Case/Join directly
/// (emit_tail_app / emit_case+Tail / emit_join+Tail) and only ever re-enters this
/// hylomorphism for value positions. The `collapse_frame` Case/Join branches thus
/// always run NonTail here; the spine supplies Tail to their alts/body separately.
fn emit_subtree_with_tail(args: EmitArgs, idx: usize) -> Result<SsaVal, EmitError> {
    try_expand_and_collapse::<EmitFrameToken, _, _, _>(
        idx,
        |idx| expand_node(args.sess.tree, idx),
        |frame| {
            collapse_frame(
                EmitArgs {
                    ctx: args.ctx,
                    sess: args.sess,
                    builder: args.builder,
                    // Hard NonTail: never propagate the caller's tail into
                    // value-position children. The spine owns tail (see above).
                    tail: TailCtx::NonTail,
                },
                frame,
            )
        },
    )
}

// ---------------------------------------------------------------------------
// Cheapness analysis: decide which Con fields need thunks
// ---------------------------------------------------------------------------

/// Returns true if the expression at `idx` is trivial (safe to evaluate eagerly).
/// Trivial expressions are already in WHNF or produce values with no computation.
fn is_trivial_field(idx: usize, expr: &CoreExpr) -> bool {
    match &expr.nodes[idx] {
        CoreFrame::Var(_) => true,
        CoreFrame::Lit(_) => true,
        CoreFrame::Lam { .. } => true, // Already WHNF (closure)
        CoreFrame::Con { fields, .. } => fields.iter().all(|&f| is_trivial_field(f, expr)),
        CoreFrame::PrimOp { args, .. } => args.iter().all(|&a| is_trivial_field(a, expr)),
        _ => false, // App, Case, LetNonRec, LetRec, Join, Jump
    }
}

// ---------------------------------------------------------------------------
// Lam compilation helper (extracted for readability)
// ---------------------------------------------------------------------------

/// Compute sorted capture list for a closure/thunk body.
/// If `exclude` is Some, that VarId is removed from free vars (for lambda binders).
fn compute_captures(
    ctx: &EmitContext,
    tree: &CoreExpr,
    body_idx: usize,
    exclude: Option<VarId>,
    label: &str,
) -> (CoreExpr, Vec<VarId>) {
    let body_tree = tree.extract_subtree(body_idx);
    let fvs = tidepool_repr::free_vars::free_vars(&body_tree);

    let dropped: Vec<VarId> = fvs
        .iter()
        .filter(|v| !ctx.env.contains_key(v))
        .copied()
        .collect();
    if !dropped.is_empty() && crate::debug::trace_level() >= crate::debug::TraceLevel::Scope {
        ctx.trace_scope(&format!(
            "{} capture: dropped {} free vars not in scope: {:?}",
            label,
            dropped.len(),
            dropped
        ));
    }
    let mut sorted_fvs: Vec<VarId> = fvs
        .into_iter()
        .filter(|v| ctx.env.contains_key(v))
        .collect();

    if let Some(binder) = exclude {
        if let Ok(idx) = sorted_fvs.binary_search(&binder) {
            sorted_fvs.remove(idx);
        }
    }
    (body_tree, sorted_fvs)
}

fn emit_lam(args: EmitArgs, binder: VarId, body_idx: usize) -> Result<SsaVal, EmitError> {
    let (body_tree, sorted_fvs) =
        compute_captures(args.ctx, args.sess.tree, body_idx, Some(binder), "lam");

    let captures: Vec<(VarId, SsaVal)> = sorted_fvs
        .iter()
        .map(|v| {
            let val = args.ctx.env.get(v).ok_or_else(|| {
                EmitError::MissingCaptureVar(
                    *v,
                    format!(
                        "Lam capture: not in env (env has {} vars)",
                        args.ctx.env.len()
                    ),
                )
            })?;
            Ok::<_, EmitError>((*v, *val))
        })
        .collect::<Result<Vec<_>, EmitError>>()?;

    let lambda_name = args.ctx.next_lambda_name();
    let mut closure_sig = Signature::new(args.sess.pipeline.isa.default_call_conv());
    closure_sig.params.push(AbiParam::new(types::I64)); // vmctx
    closure_sig.params.push(AbiParam::new(types::I64)); // self
    closure_sig.params.push(AbiParam::new(types::I64)); // arg
    closure_sig.returns.push(AbiParam::new(types::I64));

    let lambda_func_id = args
        .sess
        .pipeline
        .module
        .declare_function(&lambda_name, Linkage::Local, &closure_sig)
        .map_err(|e| EmitError::CraneliftError(e.to_string()))?;
    args.sess
        .pipeline
        .register_lambda(lambda_func_id, lambda_name.clone());
    if crate::debug::trace_level() >= crate::debug::TraceLevel::Calls {
        eprintln!("[emit] {} binder={:#x}", lambda_name, binder.0);
    }

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

    let mut inner_gc_sig = Signature::new(args.sess.pipeline.isa.default_call_conv());
    inner_gc_sig.params.push(AbiParam::new(types::I64));
    let inner_gc_sig_ref = inner_builder.import_signature(inner_gc_sig);

    let inner_oom_func = {
        let mut sig = Signature::new(args.sess.pipeline.isa.default_call_conv());
        sig.returns.push(AbiParam::new(types::I64));
        let func_id = args
            .sess
            .pipeline
            .module
            .declare_function("runtime_oom", Linkage::Import, &sig)
            .map_err(|e| EmitError::CraneliftError(format!("declare runtime_oom: {e}")))?;
        args.sess
            .pipeline
            .module
            .declare_func_in_func(func_id, inner_builder.func)
    };

    let mut inner_emit = EmitContext::new(args.ctx.prefix.clone());
    inner_emit.lambda_counter = args.ctx.lambda_counter;
    inner_emit.current_fn = lambda_name.clone();

    if crate::debug::trace_level() >= crate::debug::TraceLevel::Scope {
        inner_emit.trace_scope(&format!("insert lam binder {:?}", binder));
    }
    inner_emit.env.insert(binder, SsaVal::HeapPtr(arg_param));

    for (i, (var_id, _)) in captures.iter().enumerate() {
        let offset = CLOSURE_CAPTURED_OFFSET + 8 * i as i32;
        let val = inner_builder
            .ins()
            .load(types::I64, MemFlags::trusted(), closure_self, offset);
        inner_builder.declare_value_needs_stack_map(val);
        if crate::debug::trace_level() >= crate::debug::TraceLevel::Scope {
            inner_emit.trace_scope(&format!("insert lam capture {:?}", var_id));
        }
        inner_emit.env.insert(*var_id, SsaVal::HeapPtr(val));
    }

    let body_root = body_tree.nodes.len() - 1;
    let mut inner_sess = EmitSession {
        pipeline: args.sess.pipeline,
        vmctx: inner_vmctx,
        gc_sig: inner_gc_sig_ref,
        oom_func: inner_oom_func,
        tree: &body_tree,
        lit_wrappers: args.sess.lit_wrappers,
    };
    let body_result = EmitContext::emit_node(
        EmitArgs {
            ctx: &mut inner_emit,
            sess: &mut inner_sess,
            builder: &mut inner_builder,
            tail: TailCtx::Tail,
        },
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

    args.ctx.lambda_counter = inner_emit.lambda_counter;

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

    args.sess
        .pipeline
        .define_function(lambda_func_id, &mut inner_ctx)?;

    let func_ref = args
        .sess
        .pipeline
        .module
        .declare_func_in_func(lambda_func_id, args.builder.func);
    let code_ptr = args.builder.ins().func_addr(types::I64, func_ref);

    let num_captures = captures.len();
    let closure_size = 24 + 8 * num_captures as u64;
    let closure_ptr = emit_alloc_fast_path(
        args.builder,
        args.sess.vmctx,
        closure_size,
        args.sess.gc_sig,
        args.sess.oom_func,
    );

    let tag_val = args
        .builder
        .ins()
        .iconst(types::I8, layout::TAG_CLOSURE as i64);
    args.builder
        .ins()
        .store(MemFlags::trusted(), tag_val, closure_ptr, 0);
    let size_val = args.builder.ins().iconst(types::I16, closure_size as i64);
    args.builder
        .ins()
        .store(MemFlags::trusted(), size_val, closure_ptr, 1);

    args.builder.ins().store(
        MemFlags::trusted(),
        code_ptr,
        closure_ptr,
        CLOSURE_CODE_PTR_OFFSET,
    );
    let num_cap_val = args.builder.ins().iconst(types::I16, num_captures as i64);
    args.builder.ins().store(
        MemFlags::trusted(),
        num_cap_val,
        closure_ptr,
        CLOSURE_NUM_CAPTURED_OFFSET,
    );

    for (i, (_, ssaval)) in captures.iter().enumerate() {
        let cap_val = ensure_heap_ptr(
            args.builder,
            args.sess.vmctx,
            args.sess.gc_sig,
            args.sess.oom_func,
            *ssaval,
        );
        let offset = CLOSURE_CAPTURED_OFFSET + 8 * i as i32;
        args.builder
            .ins()
            .store(MemFlags::trusted(), cap_val, closure_ptr, offset);
    }

    args.builder.declare_value_needs_stack_map(closure_ptr);
    Ok(SsaVal::HeapPtr(closure_ptr))
}

// ---------------------------------------------------------------------------
// Thunk compilation helper
// ---------------------------------------------------------------------------

/// Compile a non-trivial sub-expression as a thunk: a separate Cranelift function
/// with signature `(vmctx: i64, thunk_ptr: i64) -> i64` that loads captures from
/// the thunk object and evaluates the deferred expression. Returns the allocated
/// thunk heap pointer.
///
/// The thunk entry function is a pure computation \u2014 `heap_force` handles the
/// state machine (blackhole, call entry, write indirection, set evaluated).
fn emit_thunk(args: EmitArgs, body_idx: usize) -> Result<SsaVal, EmitError> {
    // Extract the sub-expression and compute free variables
    let (body_tree, sorted_fvs) =
        compute_captures(args.ctx, args.sess.tree, body_idx, None, "thunk");

    let captures: Vec<(VarId, SsaVal)> = sorted_fvs
        .iter()
        .map(|v| {
            let val = args.ctx.env.get(v).ok_or_else(|| {
                EmitError::MissingCaptureVar(
                    *v,
                    format!(
                        "Thunk capture: not in env (env has {} vars)",
                        args.ctx.env.len()
                    ),
                )
            })?;
            Ok::<_, EmitError>((*v, *val))
        })
        .collect::<Result<Vec<_>, EmitError>>()?;

    // Declare the thunk entry function: (vmctx, thunk_ptr) -> result
    let thunk_name = args.ctx.next_thunk_name();
    let mut thunk_sig = Signature::new(args.sess.pipeline.isa.default_call_conv());
    thunk_sig.params.push(AbiParam::new(types::I64)); // vmctx
    thunk_sig.params.push(AbiParam::new(types::I64)); // thunk_ptr (self)
    thunk_sig.returns.push(AbiParam::new(types::I64));

    let thunk_func_id = args
        .sess
        .pipeline
        .module
        .declare_function(&thunk_name, Linkage::Local, &thunk_sig)
        .map_err(|e| EmitError::CraneliftError(e.to_string()))?;
    args.sess
        .pipeline
        .register_lambda(thunk_func_id, thunk_name.clone());

    // Build the inner function
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

    let mut inner_gc_sig = Signature::new(args.sess.pipeline.isa.default_call_conv());
    inner_gc_sig.params.push(AbiParam::new(types::I64));
    let inner_gc_sig_ref = inner_builder.import_signature(inner_gc_sig);

    let inner_oom_func = {
        let mut sig = Signature::new(args.sess.pipeline.isa.default_call_conv());
        sig.returns.push(AbiParam::new(types::I64));
        let func_id = args
            .sess
            .pipeline
            .module
            .declare_function("runtime_oom", Linkage::Import, &sig)
            .map_err(|e| EmitError::CraneliftError(format!("declare runtime_oom: {e}")))?;
        args.sess
            .pipeline
            .module
            .declare_func_in_func(func_id, inner_builder.func)
    };

    let mut inner_emit = EmitContext::new(args.ctx.prefix.clone());
    inner_emit.lambda_counter = args.ctx.lambda_counter;
    inner_emit.current_fn = thunk_name.clone();

    // Load captures from thunk object: thunk_ptr + THUNK_CAPTURED_OFFSET + 8*i
    for (i, (var_id, _)) in captures.iter().enumerate() {
        let offset = THUNK_CAPTURED_OFFSET + 8 * i as i32;
        let val = inner_builder
            .ins()
            .load(types::I64, MemFlags::trusted(), thunk_self, offset);
        inner_builder.declare_value_needs_stack_map(val);
        if crate::debug::trace_level() >= crate::debug::TraceLevel::Scope {
            inner_emit.trace_scope(&format!("insert thunk capture {:?}", var_id));
        }
        inner_emit.env.insert(*var_id, SsaVal::HeapPtr(val));
    }

    // Emit the deferred expression body
    let body_root = body_tree.nodes.len() - 1;
    let mut inner_sess = EmitSession {
        pipeline: args.sess.pipeline,
        vmctx: inner_vmctx,
        gc_sig: inner_gc_sig_ref,
        oom_func: inner_oom_func,
        tree: &body_tree,
        lit_wrappers: args.sess.lit_wrappers,
    };
    let body_result = EmitContext::emit_node(
        EmitArgs {
            ctx: &mut inner_emit,
            sess: &mut inner_sess,
            builder: &mut inner_builder,
            tail: TailCtx::NonTail,
        },
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

    args.ctx.lambda_counter = inner_emit.lambda_counter;

    // Debug: dump Cranelift IR for thunk when TIDEPOOL_DUMP_CLIF=1
    if std::env::var("TIDEPOOL_DUMP_CLIF").is_ok() {
        eprintln!("=== CLIF {} ({} captures) ===", thunk_name, captures.len());
        for (i, (var_id, ssaval)) in captures.iter().enumerate() {
            let kind = match ssaval {
                SsaVal::HeapPtr(_) => "HeapPtr",
                SsaVal::Raw(_, tag) => &format!("Raw(tag={})", tag),
            };
            eprintln!("  capture[{}]: VarId({:#x}) = {}", i, var_id.0, kind);
        }
        eprintln!("{}", inner_ctx.func.display());
        eprintln!("=== END CLIF {} ===", thunk_name);
    }

    args.sess
        .pipeline
        .define_function(thunk_func_id, &mut inner_ctx)?;

    // Get code pointer in the parent function
    let func_ref = args
        .sess
        .pipeline
        .module
        .declare_func_in_func(thunk_func_id, args.builder.func);
    let code_ptr = args.builder.ins().func_addr(types::I64, func_ref);

    // Allocate the thunk heap object
    let num_captures = captures.len();
    let thunk_size = 24 + 8 * num_captures as u64;
    let thunk_ptr = emit_alloc_fast_path(
        args.builder,
        args.sess.vmctx,
        thunk_size,
        args.sess.gc_sig,
        args.sess.oom_func,
    );

    // Header: tag + size
    let tag_val = args
        .builder
        .ins()
        .iconst(types::I8, layout::TAG_THUNK as i64);
    args.builder
        .ins()
        .store(MemFlags::trusted(), tag_val, thunk_ptr, 0);
    let size_val = args.builder.ins().iconst(types::I16, thunk_size as i64);
    args.builder
        .ins()
        .store(MemFlags::trusted(), size_val, thunk_ptr, 1);

    // State = Unevaluated
    let state_val = args
        .builder
        .ins()
        .iconst(types::I8, layout::THUNK_UNEVALUATED as i64);
    args.builder.ins().store(
        MemFlags::trusted(),
        state_val,
        thunk_ptr,
        THUNK_STATE_OFFSET,
    );

    // Code pointer
    args.builder.ins().store(
        MemFlags::trusted(),
        code_ptr,
        thunk_ptr,
        THUNK_CODE_PTR_OFFSET,
    );

    // Store captures
    for (i, (_, ssaval)) in captures.iter().enumerate() {
        let cap_val = ensure_heap_ptr(
            args.builder,
            args.sess.vmctx,
            args.sess.gc_sig,
            args.sess.oom_func,
            *ssaval,
        );
        let offset = THUNK_CAPTURED_OFFSET + 8 * i as i32;
        args.builder
            .ins()
            .store(MemFlags::trusted(), cap_val, thunk_ptr, offset);
    }

    args.builder.declare_value_needs_stack_map(thunk_ptr);
    Ok(SsaVal::HeapPtr(thunk_ptr))
}

/// Bind an error-call RHS without evaluating it. Preference order:
/// 1. Message statically extractable -> message-carrying lazy poison.
/// 2. Otherwise -> a real thunk: forcing it executes the Raise, which
///    materializes the message from the live argument at runtime
///    (`runtime_error_dynamic`).
/// 3. Thunk captures unavailable in this context (subtree-severed sibling
///    deps) -> message-less lazy poison, the old behavior.
fn emit_error_binding(args: EmitArgs, rhs_idx: usize) -> Result<SsaVal, EmitError> {
    let kind = EmitContext::extract_error_kind(args.sess.tree, rhs_idx);
    if let Some(msg) = EmitContext::extract_error_message(args.sess.tree, rhs_idx) {
        let addr = crate::host_fns::error_poison_ptr_lazy_msg(kind, &msg) as i64;
        let v = args.builder.ins().iconst(types::I64, addr);
        return Ok(SsaVal::HeapPtr(v));
    }
    // Bare sentinel Var: no message expression exists anywhere — the poison
    // value is exact (and self-swallows non-string args if applied later).
    if matches!(&args.sess.tree.nodes[rhs_idx], CoreFrame::Var(_)) {
        let addr = crate::host_fns::error_poison_ptr_lazy(kind) as i64;
        let v = args.builder.ins().iconst(types::I64, addr);
        return Ok(SsaVal::HeapPtr(v));
    }
    match emit_thunk(
        EmitArgs {
            ctx: args.ctx,
            sess: args.sess,
            builder: args.builder,
            tail: TailCtx::NonTail,
        },
        rhs_idx,
    ) {
        Ok(v) => Ok(v),
        Err(EmitError::MissingCaptureVar(_, _)) => {
            let addr = crate::host_fns::error_poison_ptr_lazy(kind) as i64;
            let v = args.builder.ins().iconst(types::I64, addr);
            Ok(SsaVal::HeapPtr(v))
        }
        Err(e) => Err(e),
    }
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

    // Carried from the pipeline (set by the JIT entry point from the
    // DataConTable; defaults to empty for direct test callers of compile_expr).
    let lit_wrappers = pipeline.lit_wrappers;
    let mut sess = EmitSession {
        pipeline,
        vmctx,
        gc_sig: gc_sig_ref,
        oom_func,
        tree,
        lit_wrappers,
    };

    let result = EmitContext::emit_node(
        EmitArgs {
            ctx: &mut emit_ctx,
            sess: &mut sess,
            builder: &mut builder,
            tail: TailCtx::NonTail,
        },
        tree.nodes.len() - 1,
    )?;
    let ret = ensure_heap_ptr(&mut builder, vmctx, gc_sig_ref, oom_func, result);

    builder.ins().return_(&[ret]);
    builder.finalize();

    pipeline.define_function(func_id, &mut ctx)?;

    Ok(func_id)
}

impl EmitContext {
    /// Check if a binding's RHS references an error sentinel (tag ERROR_SENTINEL_TAG).
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
                CoreFrame::Var(v) => return (v.0 >> 56) as u8 == tidepool_repr::ERROR_SENTINEL_TAG,
                CoreFrame::App { fun, .. } => idx = *fun,
                // GHC gives a bottoming binding of unlifted type the shape
                // `case error "..." of {}` (an empty case forcing the error to
                // realise its unlifted result type — e.g. roundingMode#'s
                // `IN -> error` lifted to a top-level `Int#` CAF). Forcing the
                // binding forces the case SCRUTINEE first, so if that bottoms the
                // binding bottoms — follow the scrutinee (NOT the alt bodies,
                // which are conditional, keeping branch-local errors un-poisoned).
                CoreFrame::Case { scrutinee, .. } => idx = *scrutinee,
                // `raise# exc` (GHC's primitive exception throw, e.g. the
                // overflow / ratioZeroDenominator path in rationalToDouble) is a
                // bottoming RHS just like `error …`. As an inline expression it is
                // lowered to a conditional `EmitFrame::Raise`, but as a LetRec
                // SIMPLE binding the strict spine would evaluate it eagerly and
                // throw regardless of control flow unless it is deferred here.
                CoreFrame::PrimOp {
                    op: PrimOpKind::Raise,
                    ..
                } => return true,
                _ => return false,
            }
        }
    }

    /// Group-aware variant of `rhs_is_error_call`: additionally follows the
    /// App-spine head through SIBLING binders of the same binding group.
    ///
    /// GHC .hi unfoldings float error CAFs in two steps, e.g. for the
    /// Foldable [] instance of foldl1:
    ///   lvl  = \cs -> error (pushCallStack ... cs) "foldl1"   -- lambda: safe
    ///   lvl2 = lvl callStackValue                             -- App CAF: bottom!
    /// The plain check sees `lvl2`'s head var is not a sentinel and lets the
    /// eager Let spine evaluate it at setup — raising "foldl1" before any case
    /// dispatch ever runs. Following the head into `lvl`'s RHS (stripping its
    /// lambda binders, since reaching it via an App spine means they get
    /// applied) finds the sentinel and defers the binding instead.
    ///
    /// A `Lam` RHS at the top level is still NOT an error call (binding a
    /// lambda value is always safe); lambdas are only stripped after entering
    /// a sibling via its use in head position. Cycle-guarded via `visited`.
    fn rhs_is_error_call_in_group(
        tree: &CoreExpr,
        rhs_idx: usize,
        group: &FxHashMap<VarId, usize>,
    ) -> bool {
        let mut visited: FxHashSet<VarId> = FxHashSet::default();
        let mut idx = rhs_idx;
        loop {
            match &tree.nodes[idx] {
                CoreFrame::Var(v) => {
                    if (v.0 >> 56) as u8 == tidepool_repr::ERROR_SENTINEL_TAG {
                        return true;
                    }
                    match group.get(v) {
                        Some(&sib_rhs) if visited.insert(*v) => {
                            idx = sib_rhs;
                            while let CoreFrame::Lam { body, .. } = &tree.nodes[idx] {
                                idx = *body;
                            }
                        }
                        _ => return false,
                    }
                }
                CoreFrame::App { fun, .. } => idx = *fun,
                // See `rhs_is_error_call`: a `case <error> of {}` bottoms via its
                // forced scrutinee; `raise# exc` is a bottoming RHS.
                CoreFrame::Case { scrutinee, .. } => idx = *scrutinee,
                CoreFrame::PrimOp {
                    op: PrimOpKind::Raise,
                    ..
                } => return true,
                _ => return false,
            }
        }
    }

    /// Extract the error kind from an error call (walks App chain to find head Var).
    fn extract_error_kind(tree: &CoreExpr, rhs_idx: usize) -> u64 {
        let mut idx = rhs_idx;
        loop {
            match &tree.nodes[idx] {
                CoreFrame::Var(v) if (v.0 >> 56) as u8 == tidepool_repr::ERROR_SENTINEL_TAG => {
                    return v.0 & 0xFF
                }
                CoreFrame::App { fun, .. } => idx = *fun,
                CoreFrame::Case { scrutinee, .. } => idx = *scrutinee,
                _ => return 2, // fallback: UserError
            }
        }
    }

    /// Extract the error message from an error call (walks App chain to find LitString).
    fn extract_error_message(tree: &CoreExpr, rhs_idx: usize) -> Option<Vec<u8>> {
        let mut idx = rhs_idx;
        loop {
            match &tree.nodes[idx] {
                CoreFrame::App { fun, arg } => {
                    // The message is rarely a bare LitString: the Text-typed
                    // `error` shadow produces shapes like `error (unpack "msg")`
                    // and OverloadedStrings literals arrive via pack/unpackCString#
                    // wrappers. Scan the argument subtree for the first string
                    // literal instead of requiring an exact shape.
                    if let Some(bytes) = Self::find_first_lit_string(tree, *arg) {
                        return Some(bytes);
                    }
                    idx = *fun; // continue walking the App chain
                }
                // `case error "..." of {}`: the message is inside the scrutinee.
                CoreFrame::Case { scrutinee, .. } => idx = *scrutinee,
                _ => return None,
            }
        }
    }

    /// Bounded DFS over a subtree for the first `LitString`. Error-call
    /// arguments are tiny; the node budget only guards against scanning a
    /// large unrelated expression that happens to sit in argument position.
    /// `Var` references are resolved through let-bindings (one extra lookup
    /// pass, built lazily): GHC floats message literals to outer bindings in
    /// larger modules, so the literal is often behind `error (unpack lvl)`.
    fn find_first_lit_string(tree: &CoreExpr, root: usize) -> Option<Vec<u8>> {
        const NODE_BUDGET: usize = 64;
        let mut binder_rhs: Option<std::collections::HashMap<VarId, usize>> = None;
        let mut stack = vec![root];
        let mut visited = 0usize;
        while let Some(i) = stack.pop() {
            visited += 1;
            if visited > NODE_BUDGET {
                return None;
            }
            match &tree.nodes[i] {
                CoreFrame::Lit(Literal::LitString(bytes)) => return Some(bytes.clone()),
                CoreFrame::Var(v) => {
                    // Resolve through let-bound vars (floated literals).
                    let map = binder_rhs.get_or_insert_with(|| {
                        let mut m = std::collections::HashMap::new();
                        for node in &tree.nodes {
                            match node {
                                CoreFrame::LetNonRec { binder, rhs, .. } => {
                                    m.insert(*binder, *rhs);
                                }
                                CoreFrame::LetRec { bindings, .. } => {
                                    m.extend(bindings.iter().map(|(b, r)| (*b, *r)));
                                }
                                _ => {}
                            }
                        }
                        m
                    });
                    if let Some(rhs) = map.get(v) {
                        stack.push(*rhs);
                    }
                }
                CoreFrame::Lit(_) => {}
                CoreFrame::App { fun, arg } => {
                    stack.push(*fun);
                    stack.push(*arg);
                }
                CoreFrame::Lam { body, .. } => stack.push(*body),
                CoreFrame::LetNonRec { rhs, body, .. } => {
                    stack.push(*rhs);
                    stack.push(*body);
                }
                CoreFrame::LetRec { bindings, body } => {
                    stack.extend(bindings.iter().map(|(_, r)| *r));
                    stack.push(*body);
                }
                CoreFrame::Case {
                    scrutinee, alts, ..
                } => {
                    stack.push(*scrutinee);
                    stack.extend(alts.iter().map(|a| a.body));
                }
                CoreFrame::Con { fields, .. } => stack.extend(fields.iter().copied()),
                CoreFrame::Join { rhs, body, .. } => {
                    stack.push(*rhs);
                    stack.push(*body);
                }
                CoreFrame::Jump { args, .. } => stack.extend(args.iter().copied()),
                CoreFrame::PrimOp { args, .. } => stack.extend(args.iter().copied()),
            }
        }
        None
    }

    /// Trampoline-based emit_node: converts recursive Let-chain evaluation to
    /// an explicit work stack. This prevents Rust stack overflow during JIT
    /// compilation of deeply nested GHC Core ASTs.
    ///
    /// Recursive calls that remain (bounded, safe):
    /// - emit_lam/emit_thunk: create new EmitContext, bounded by lambda nesting
    /// - emit_case/emit_join: called from hylomorphism collapse, bounded by case nesting
    /// - Trivial Con field eval: constant stack depth (Var/Lit)
    pub fn emit_node(args: EmitArgs, root_idx: usize) -> Result<SsaVal, EmitError> {
        // Stack-growth insurance at the emit recursion spine.
        //
        // `emit_node`'s Let chain is already trampolined onto an explicit work
        // stack, but case-ALT body emission still re-enters `emit_node`
        // natively (emit_node → emit_case/dispatch → emit_node), so deeply
        // case-nested programs grow the call stack ~one large frame per level.
        // That is the ~2 MiB cliff measured in plans/stack-safety.md. The
        // production/proptest path already runs emit on a large worker stack;
        // this `maybe_grow` is the cheap interim guarantee for any path where
        // that discipline slips — if the remaining red zone is below 64 KiB it
        // allocates a fresh 4 MiB segment and continues there. Cost is ~nil
        // when there is ample stack, so it is left unconditional.
        //
        // 64 KiB red zone / 4 MiB growth (the rustc defaults).
        stacker::maybe_grow(64 * 1024, 4 * 1024 * 1024, move || {
            Self::emit_node_impl(args, root_idx)
        })
    }

    fn emit_node_impl(args: EmitArgs, root_idx: usize) -> Result<SsaVal, EmitError> {
        let mut work: Vec<EmitWork> = vec![EmitWork::Eval(root_idx, args.tail)];
        let mut vals: Vec<SsaVal> = Vec::new();

        while let Some(item) = work.pop() {
            match item {
                EmitWork::Eval(start_idx, tail_ctx) => {
                    // Inner iterative loop: skip through Let chains in tail position
                    let mut idx = start_idx;
                    loop {
                        match &args.sess.tree.nodes[idx] {
                            CoreFrame::LetNonRec { binder, rhs, body } => {
                                crate::coverage::hit("frame:LetNonRec");
                                let binder = *binder;
                                let rhs = *rhs;
                                let body = *body;
                                // Dead code elimination: skip RHS if binder is unused in body.
                                let body_fvs = tidepool_repr::free_vars::free_vars(
                                    &args.sess.tree.extract_subtree(body),
                                );
                                if body_fvs.binary_search(&binder).is_ok() {
                                    if Self::rhs_is_error_call(args.sess.tree, rhs) {
                                        // Bind lazily: error only triggers on force/call.
                                        let poison_sv = emit_error_binding(
                                            EmitArgs {
                                                ctx: args.ctx,
                                                sess: args.sess,
                                                builder: args.builder,
                                                tail: TailCtx::NonTail,
                                            },
                                            rhs,
                                        )?;
                                        if crate::debug::trace_level()
                                            >= crate::debug::TraceLevel::Scope
                                        {
                                            args.ctx.trace_scope(&format!(
                                                "defer error LetNonRec {:?}",
                                                binder
                                            ));
                                        }
                                        let old_val = args.ctx.env.insert(binder, poison_sv);
                                        // No RHS eval needed, just push cleanup and continue to body
                                        work.push(EmitWork::LetCleanupMark(LetCleanup::Single(
                                            binder, old_val,
                                        )));
                                    } else {
                                        // Push work in LIFO order: cleanup, eval body, bind, eval rhs
                                        // After rhs eval \u2192 bind \u2192 eval body \u2192 cleanup
                                        let old_val = args.ctx.env.get(&binder).cloned();
                                        work.push(EmitWork::LetCleanupMark(LetCleanup::Single(
                                            binder, old_val,
                                        )));
                                        work.push(EmitWork::Eval(body, tail_ctx));
                                        work.push(EmitWork::Bind(binder));
                                        work.push(EmitWork::Eval(rhs, TailCtx::NonTail));
                                        break; // exit inner loop, process work stack
                                    }
                                } else if crate::debug::trace_level()
                                    >= crate::debug::TraceLevel::Scope
                                {
                                    args.ctx
                                        .trace_scope(&format!("DCE skip LetNonRec {:?}", binder));
                                }
                                idx = body;
                                continue;
                            }
                            CoreFrame::LetRec { bindings, body } => {
                                crate::coverage::hit("frame:LetRec");
                                let bindings = bindings.clone();
                                let body = *body;
                                // Run phases 1-3b inline, push deferred evals + finish + cleanup
                                let mut scope = EnvScope::new();
                                for (b, _) in &bindings {
                                    scope.saved.push((*b, args.ctx.env.get(b).copied()));
                                }
                                work.push(EmitWork::LetCleanupMark(LetCleanup::Rec(scope)));
                                Self::emit_letrec_phases(
                                    EmitArgs {
                                        ctx: args.ctx,
                                        sess: args.sess,
                                        builder: args.builder,
                                        tail: tail_ctx,
                                    },
                                    &bindings,
                                    body,
                                    &mut work,
                                )?;
                                break; // exit inner loop
                            }
                            // All non-Let nodes. Tail-ness propagates ONLY along
                            // the evaluation spine — never into value positions.
                            // The hylomorphism (emit_subtree) is hard-NonTail, so a
                            // tail App/Case/Join must be dispatched HERE to keep TCO;
                            // everything else cannot tail-call and is emitted NonTail.
                            // (Fixes #313 t11: a tail leak into a value-position join
                            // made a tail App `return null`, escaping the function.)
                            _ => {
                                if tail_ctx.is_tail() {
                                    match &args.sess.tree.nodes[idx] {
                                        CoreFrame::App { .. } => {
                                            let result = Self::emit_tail_app(
                                                EmitArgs {
                                                    ctx: args.ctx,
                                                    sess: args.sess,
                                                    builder: args.builder,
                                                    tail: tail_ctx,
                                                },
                                                idx,
                                            )?;
                                            vals.push(result);
                                        }
                                        CoreFrame::Case {
                                            scrutinee,
                                            binder,
                                            alts,
                                        } => {
                                            // Scrutinee is a value position (NonTail,
                                            // via emit_subtree); only the alts inherit
                                            // Tail, preserving case-alt TCO.
                                            let scrutinee = *scrutinee;
                                            let binder = *binder;
                                            let alts = alts.clone();
                                            let scrut = emit_subtree(
                                                EmitArgs {
                                                    ctx: args.ctx,
                                                    sess: args.sess,
                                                    builder: args.builder,
                                                    tail: TailCtx::NonTail,
                                                },
                                                scrutinee,
                                            )?;
                                            let result = crate::emit::case::emit_case(
                                                EmitArgs {
                                                    ctx: args.ctx,
                                                    sess: args.sess,
                                                    builder: args.builder,
                                                    tail: TailCtx::Tail,
                                                },
                                                scrut,
                                                &binder,
                                                &alts,
                                            )?;
                                            vals.push(result);
                                        }
                                        CoreFrame::Join {
                                            label,
                                            params,
                                            rhs,
                                            body,
                                        } => {
                                            // Join body and rhs are the spine (both
                                            // produce the join's value); emit_join
                                            // propagates Tail to them and keeps jump
                                            // args NonTail.
                                            let label = *label;
                                            let params = params.clone();
                                            let rhs = *rhs;
                                            let body = *body;
                                            let result = crate::emit::join::emit_join(
                                                EmitArgs {
                                                    ctx: args.ctx,
                                                    sess: args.sess,
                                                    builder: args.builder,
                                                    tail: TailCtx::Tail,
                                                },
                                                &label,
                                                &params,
                                                rhs,
                                                body,
                                            )?;
                                            vals.push(result);
                                        }
                                        // Con/Lit/Var/PrimOp/Jump/etc: no tail call is
                                        // possible, so NonTail is equivalent and keeps
                                        // any nested value-position Case/Join NonTail.
                                        _ => {
                                            let result = emit_subtree(
                                                EmitArgs {
                                                    ctx: args.ctx,
                                                    sess: args.sess,
                                                    builder: args.builder,
                                                    tail: TailCtx::NonTail,
                                                },
                                                idx,
                                            )?;
                                            vals.push(result);
                                        }
                                    }
                                } else {
                                    let result = emit_subtree(
                                        EmitArgs {
                                            ctx: args.ctx,
                                            sess: args.sess,
                                            builder: args.builder,
                                            tail: TailCtx::NonTail,
                                        },
                                        idx,
                                    )?;
                                    vals.push(result);
                                }
                                break;
                            }
                        }
                    }
                }
                EmitWork::Bind(binder) => {
                    let val = vals.pop().ok_or_else(|| {
                        EmitError::InternalError("Bind: empty value stack".into())
                    })?;
                    if crate::debug::trace_level() >= crate::debug::TraceLevel::Scope {
                        args.ctx
                            .trace_scope(&format!("insert LetNonRec {:?}", binder));
                    }
                    args.ctx.env.insert(binder, val);
                }
                EmitWork::LetRecPostSimple { binder, state_idx } => {
                    let val = vals.pop().ok_or_else(|| {
                        EmitError::InternalError("LetRecPostSimple: empty value stack".into())
                    })?;
                    if crate::debug::trace_level() >= crate::debug::TraceLevel::Scope {
                        args.ctx
                            .trace_scope(&format!("insert LetRec(simple) {:?}", binder));
                    }
                    args.ctx.env.insert(binder, val);
                    Self::letrec_post_simple_step(
                        EmitArgs {
                            ctx: args.ctx,
                            sess: args.sess,
                            builder: args.builder,
                            tail: TailCtx::NonTail,
                        },
                        &binder,
                        state_idx,
                    )?;
                }
                EmitWork::LetRecFinish {
                    body,
                    state_idx,
                    tail,
                } => {
                    Self::letrec_finish_phases(
                        EmitArgs {
                            ctx: args.ctx,
                            sess: args.sess,
                            builder: args.builder,
                            tail: TailCtx::NonTail,
                        },
                        state_idx,
                    )?;
                    // Push body evaluation
                    work.push(EmitWork::Eval(body, tail));
                }
                EmitWork::LetCleanupMark(cleanup) => match cleanup {
                    LetCleanup::Single(var, old_val) => {
                        if crate::debug::trace_level() >= crate::debug::TraceLevel::Scope {
                            args.ctx
                                .trace_scope(&format!("restore LetCleanup {:?}", var));
                        }
                        args.ctx.env.restore(var, old_val);
                    }
                    LetCleanup::Rec(scope) => {
                        args.ctx.trace_scope("restore LetCleanup(rec)");
                        args.ctx.env.restore_scope(scope);
                    }
                },
            }
        }

        vals.pop()
            .ok_or_else(|| EmitError::InternalError("emit_node: empty value stack".into()))
    }

    fn emit_tail_app(args: EmitArgs, idx: usize) -> Result<SsaVal, EmitError> {
        let (fun_idx, arg_idx) = match &args.sess.tree.nodes[idx] {
            CoreFrame::App { fun, arg } => (*fun, *arg),
            other => unreachable!("emit_tail_app dispatched on non-App node: {other:?}"),
        };

        // Evaluate fun and arg in NON-tail position
        let fun_val = emit_subtree(
            EmitArgs {
                ctx: args.ctx,
                sess: args.sess,
                builder: args.builder,
                tail: TailCtx::NonTail,
            },
            fun_idx,
        )?;
        let arg_val = emit_subtree(
            EmitArgs {
                ctx: args.ctx,
                sess: args.sess,
                builder: args.builder,
                tail: TailCtx::NonTail,
            },
            arg_idx,
        )?;

        let raw_fun_ptr = fun_val.value();
        let arg_ptr = ensure_heap_ptr(
            args.builder,
            args.sess.vmctx,
            args.sess.gc_sig,
            args.sess.oom_func,
            arg_val,
        );

        // Force thunked function (same as regular App path)
        let fun_tag = args
            .builder
            .ins()
            .load(types::I8, MemFlags::trusted(), raw_fun_ptr, 0);
        let is_thunk = args.builder.ins().icmp_imm(
            IntCC::Equal,
            fun_tag,
            tidepool_heap::layout::TAG_THUNK as i64,
        );

        let force_fun_block = args.builder.create_block();
        let fun_ready_block = args.builder.create_block();
        args.builder.append_block_param(fun_ready_block, types::I64);

        args.builder.ins().brif(
            is_thunk,
            force_fun_block,
            &[],
            fun_ready_block,
            &[BlockArg::Value(raw_fun_ptr)],
        );

        args.builder.switch_to_block(force_fun_block);
        args.builder.seal_block(force_fun_block);

        let force_fn = args
            .sess
            .pipeline
            .module
            .declare_function("heap_force", Linkage::Import, &{
                let mut sig = Signature::new(args.sess.pipeline.isa.default_call_conv());
                sig.params.push(AbiParam::new(types::I64));
                sig.params.push(AbiParam::new(types::I64));
                sig.returns.push(AbiParam::new(types::I64));
                sig
            })
            .map_err(|e| EmitError::CraneliftError(e.to_string()))?;
        let force_ref = args
            .sess
            .pipeline
            .module
            .declare_func_in_func(force_fn, args.builder.func);
        let force_call = args
            .builder
            .ins()
            .call(force_ref, &[args.sess.vmctx, raw_fun_ptr]);
        let forced_fun = args.builder.inst_results(force_call)[0];
        args.builder.declare_value_needs_stack_map(forced_fun);
        args.builder
            .ins()
            .jump(fun_ready_block, &[BlockArg::Value(forced_fun)]);

        args.builder.switch_to_block(fun_ready_block);
        args.builder.seal_block(fun_ready_block);
        let fun_ptr = args.builder.block_params(fun_ready_block)[0];
        args.builder.declare_value_needs_stack_map(fun_ptr);

        // Debug validation (same as regular App)
        let check_fn = args
            .sess
            .pipeline
            .module
            .declare_function("debug_app_check", Linkage::Import, &{
                let mut sig = Signature::new(args.sess.pipeline.isa.default_call_conv());
                sig.params.push(AbiParam::new(types::I64));
                sig.returns.push(AbiParam::new(types::I64));
                sig
            })
            .map_err(|e| EmitError::CraneliftError(e.to_string()))?;
        let check_ref = args
            .sess
            .pipeline
            .module
            .declare_func_in_func(check_fn, args.builder.func);
        let check_inst = args.builder.ins().call(check_ref, &[fun_ptr]);
        let check_result = args.builder.inst_results(check_inst)[0];

        // If debug_app_check returned non-zero (poison/error), return it directly
        let store_block = args.builder.create_block();
        let poison_block = args.builder.create_block();

        let is_zero = args.builder.ins().icmp_imm(IntCC::Equal, check_result, 0);
        args.builder
            .ins()
            .brif(is_zero, store_block, &[], poison_block, &[]);

        // poison_block: return poison (error already set by debug_app_check)
        args.builder.switch_to_block(poison_block);
        args.builder.seal_block(poison_block);
        args.builder.ins().return_(&[check_result]);

        // store_block: store callee+arg to VMContext, return null
        args.builder.switch_to_block(store_block);
        args.builder.seal_block(store_block);

        // Store fun_ptr (closure) to VMContext.tail_callee (offset 24)
        args.builder.ins().store(
            MemFlags::trusted(),
            fun_ptr,
            args.sess.vmctx,
            VMCTX_TAIL_CALLEE_OFFSET,
        );
        // Store arg_ptr to VMContext.tail_arg (offset 32)
        args.builder.ins().store(
            MemFlags::trusted(),
            arg_ptr,
            args.sess.vmctx,
            VMCTX_TAIL_ARG_OFFSET,
        );

        // Return null to signal tail call
        let null_val = args.builder.ins().iconst(types::I64, 0);
        args.builder.ins().return_(&[null_val]);

        // Dead block for subsequent code
        let dead_block = args.builder.create_block();
        args.builder.switch_to_block(dead_block);
        args.builder.seal_block(dead_block);

        let dummy = args.builder.ins().iconst(types::I64, 0);
        Ok(SsaVal::HeapPtr(dummy))
    }

    /// Execute LetRec phases 1-3a inline, then push deferred-simple evals
    /// and finish onto the work stack.
    fn emit_letrec_phases(
        args: EmitArgs,
        bindings: &[(VarId, usize)],
        body: usize,
        work: &mut Vec<EmitWork>,
    ) -> Result<(), EmitError> {
        let tail = args.tail;
        // Binder → RHS map for the group-aware error-CAF check (see
        // rhs_is_error_call_in_group): floated error bindings can reference
        // the sentinel only through a sibling lambda.
        let group: FxHashMap<VarId, usize> = bindings.iter().copied().collect();
        // Split bindings: Lam/Con need 3-phase pre-allocation (recursive),
        // everything else is evaluated eagerly as simple bindings first.
        let (rec_bindings, simple_bindings): (Vec<_>, Vec<_>) =
            bindings.iter().partition(|(_, rhs_idx)| {
                matches!(
                    &args.sess.tree.nodes[*rhs_idx],
                    CoreFrame::Lam { .. } | CoreFrame::Con { .. }
                )
            });

        // If no recursive bindings, push simple evals onto work stack
        if rec_bindings.is_empty() {
            // Store empty deferred state for post-simple steps
            let state_idx = args.ctx.push_letrec_state(LetRecDeferredState {
                pending_capture_updates: FxHashMap::default(),
                deferred_con_deps: Vec::new(),
            });

            // Push finish + simple evals in reverse order (LIFO)
            work.push(EmitWork::LetRecFinish {
                body,
                state_idx,
                tail,
            });
            for (binder, rhs_idx) in simple_bindings.iter().rev() {
                if EmitContext::rhs_is_error_call_in_group(args.sess.tree, *rhs_idx, &group) {
                    let poison_sv = emit_error_binding(
                        EmitArgs {
                            ctx: args.ctx,
                            sess: args.sess,
                            builder: args.builder,
                            tail: TailCtx::NonTail,
                        },
                        *rhs_idx,
                    )?;
                    if crate::debug::trace_level() >= crate::debug::TraceLevel::Scope {
                        args.ctx
                            .trace_scope(&format!("defer error LetRec(simple) {:?}", binder));
                    }
                    args.ctx.env.insert(*binder, poison_sv);
                } else {
                    work.push(EmitWork::LetRecPostSimple {
                        binder: *binder,
                        state_idx,
                    });
                    work.push(EmitWork::Eval(*rhs_idx, TailCtx::NonTail));
                }
            }
            return Ok(());
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
            match &args.sess.tree.nodes[*rhs_idx] {
                CoreFrame::Lam {
                    binder: lam_binder,
                    body: lam_body,
                } => {
                    let lam_body_tree = args.sess.tree.extract_subtree(*lam_body);
                    let mut fvs = tidepool_repr::free_vars::free_vars(&lam_body_tree);
                    if let Ok(idx) = fvs.binary_search(lam_binder) {
                        fvs.remove(idx);
                    }
                    let dropped_fvs: Vec<VarId> = fvs
                        .iter()
                        .filter(|v| {
                            !args.ctx.env.contains_key(v)
                                && !rec_bindings.iter().any(|(b, _)| b == *v)
                                && !simple_bindings.iter().any(|(b, _)| b == *v)
                        })
                        .copied()
                        .collect();
                    if !dropped_fvs.is_empty()
                        && crate::debug::trace_level() >= crate::debug::TraceLevel::Scope
                    {
                        args.ctx.trace_scope(&format!(
                            "LetRec lam {:?}: dropped FVs {:?}",
                            binder, dropped_fvs
                        ));
                    }
                    let mut sorted_fvs: Vec<VarId> = fvs
                        .into_iter()
                        .filter(|v| {
                            args.ctx.env.contains_key(v)
                                || rec_bindings.iter().any(|(b, _)| b == v)
                                || simple_bindings.iter().any(|(b, _)| b == v)
                        })
                        .collect();
                    sorted_fvs.sort_by_key(|v| v.0);

                    let num_captures = sorted_fvs.len();
                    let closure_size = 24 + 8 * num_captures as u64;
                    let closure_ptr = emit_alloc_fast_path(
                        args.builder,
                        args.sess.vmctx,
                        closure_size,
                        args.sess.gc_sig,
                        args.sess.oom_func,
                    );

                    let tag_val = args
                        .builder
                        .ins()
                        .iconst(types::I8, layout::TAG_CLOSURE as i64);
                    args.builder
                        .ins()
                        .store(MemFlags::trusted(), tag_val, closure_ptr, 0);
                    let size_val = args.builder.ins().iconst(types::I16, closure_size as i64);
                    args.builder
                        .ins()
                        .store(MemFlags::trusted(), size_val, closure_ptr, 1);
                    let num_cap_val = args.builder.ins().iconst(types::I16, num_captures as i64);
                    args.builder.ins().store(
                        MemFlags::trusted(),
                        num_cap_val,
                        closure_ptr,
                        CLOSURE_NUM_CAPTURED_OFFSET,
                    );

                    args.builder.declare_value_needs_stack_map(closure_ptr);
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
                    let ptr = emit_alloc_fast_path(
                        args.builder,
                        args.sess.vmctx,
                        size,
                        args.sess.gc_sig,
                        args.sess.oom_func,
                    );

                    let tag_val = args.builder.ins().iconst(types::I8, layout::TAG_CON as i64);
                    args.builder
                        .ins()
                        .store(MemFlags::trusted(), tag_val, ptr, 0);
                    let size_val = args.builder.ins().iconst(types::I16, size as i64);
                    args.builder
                        .ins()
                        .store(MemFlags::trusted(), size_val, ptr, 1);
                    let con_tag_val = args.builder.ins().iconst(types::I64, tag.0 as i64);
                    args.builder
                        .ins()
                        .store(MemFlags::trusted(), con_tag_val, ptr, CON_TAG_OFFSET);
                    let num_fields_val = args.builder.ins().iconst(types::I16, num_fields as i64);
                    args.builder.ins().store(
                        MemFlags::trusted(),
                        num_fields_val,
                        ptr,
                        CON_NUM_FIELDS_OFFSET,
                    );

                    // Zero-initialize Con fields so GC doesn't trace garbage
                    // if triggered before Phase 3b/3d.
                    let null_val = args.builder.ins().iconst(types::I64, 0);
                    for i in 0..num_fields {
                        let offset = CON_FIELDS_OFFSET + 8 * i as i32;
                        args.builder
                            .ins()
                            .store(MemFlags::trusted(), null_val, ptr, offset);
                    }

                    args.builder.declare_value_needs_stack_map(ptr);
                    pre_allocs.push(PreAlloc::Con {
                        binder: *binder,
                        ptr,
                        field_indices: fields.clone(),
                    });
                }
                other => {
                    return Err(EmitError::InternalError(format!(
                        "LetRec phase 1: expected Lam or Con, got {:?}",
                        other
                    )))
                }
            }
        }

        // Phase 2: Bind all to their pre-allocated pointers
        for pa in &pre_allocs {
            let (binder, ptr) = match pa {
                PreAlloc::Lam { binder, ptr, .. } => (*binder, *ptr),
                PreAlloc::Con { binder, ptr, .. } => (*binder, *ptr),
            };
            if crate::debug::trace_level() >= crate::debug::TraceLevel::Scope {
                args.ctx
                    .trace_scope(&format!("insert LetRec(rec) {:?}", binder));
            }
            args.ctx.env.insert(binder, SsaVal::HeapPtr(ptr));
        }

        // Phase 2.5: Evaluate trivial simple bindings (Var aliases) before
        // Lam body compilation. These are just env lookups that don't depend
        // on closure code pointers.
        let mut deferred_simple = Vec::with_capacity(simple_bindings.len());
        for (binder, rhs_idx) in &simple_bindings {
            if EmitContext::rhs_is_error_call_in_group(args.sess.tree, *rhs_idx, &group) {
                let poison_sv = emit_error_binding(
                    EmitArgs {
                        ctx: args.ctx,
                        sess: args.sess,
                        builder: args.builder,
                        tail: TailCtx::NonTail,
                    },
                    *rhs_idx,
                )?;
                if crate::debug::trace_level() >= crate::debug::TraceLevel::Scope {
                    args.ctx
                        .trace_scope(&format!("defer error LetRec(trivial) {:?}", binder));
                }
                args.ctx.env.insert(*binder, poison_sv);
            } else if matches!(&args.sess.tree.nodes[*rhs_idx], CoreFrame::Var(_)) {
                // Var aliases are trivial \u2014 just an env lookup via emit_subtree
                let rhs_val = emit_subtree(
                    EmitArgs {
                        ctx: args.ctx,
                        sess: args.sess,
                        builder: args.builder,
                        tail: TailCtx::NonTail,
                    },
                    *rhs_idx,
                )?;
                if crate::debug::trace_level() >= crate::debug::TraceLevel::Scope {
                    args.ctx
                        .trace_scope(&format!("insert LetRec(trivial) {:?}", binder));
                }
                args.ctx.env.insert(*binder, rhs_val);
            } else {
                deferred_simple.push((*binder, *rhs_idx));
            }
        }

        // Phase 3a: Compile Lam bodies and set code pointers.
        // Capture VALUES are NOT filled here \u2014 some captures reference
        // deferred simple bindings (Phase 3c) that aren't in env yet.
        let mut pending_capture_updates: FxHashMap<VarId, Vec<ClosureCaptureSlot>> =
            FxHashMap::with_capacity_and_hasher(rec_bindings.len(), Default::default());

        for pa in &pre_allocs {
            let (closure_ptr, sorted_fvs, rhs_idx) = match pa {
                PreAlloc::Lam {
                    ptr, fvs, rhs_idx, ..
                } => (*ptr, fvs, *rhs_idx),
                PreAlloc::Con { .. } => continue,
            };
            let (lam_binder, lam_body) = match &args.sess.tree.nodes[rhs_idx] {
                CoreFrame::Lam { binder, body } => (*binder, *body),
                other => {
                    return Err(EmitError::InternalError(format!(
                        "LetRec phase 3a: expected Lam, got {:?}",
                        other
                    )))
                }
            };
            let lam_body_tree = args.sess.tree.extract_subtree(lam_body);

            let lambda_name = args.ctx.next_lambda_name();
            let mut closure_sig = Signature::new(args.sess.pipeline.isa.default_call_conv());
            closure_sig.params.push(AbiParam::new(types::I64));
            closure_sig.params.push(AbiParam::new(types::I64));
            closure_sig.params.push(AbiParam::new(types::I64));
            closure_sig.returns.push(AbiParam::new(types::I64));

            let lambda_func_id = args
                .sess
                .pipeline
                .module
                .declare_function(&lambda_name, Linkage::Local, &closure_sig)
                .map_err(|e| EmitError::CraneliftError(e.to_string()))?;
            args.sess
                .pipeline
                .register_lambda(lambda_func_id, lambda_name.clone());

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

            let mut inner_gc_sig = Signature::new(args.sess.pipeline.isa.default_call_conv());
            inner_gc_sig.params.push(AbiParam::new(types::I64));
            let inner_gc_sig_ref = inner_builder.import_signature(inner_gc_sig);

            let inner_oom_func = {
                let mut sig = Signature::new(args.sess.pipeline.isa.default_call_conv());
                sig.returns.push(AbiParam::new(types::I64));
                let func_id = args
                    .sess
                    .pipeline
                    .module
                    .declare_function("runtime_oom", Linkage::Import, &sig)
                    .map_err(|e| EmitError::CraneliftError(format!("declare runtime_oom: {e}")))?;
                args.sess
                    .pipeline
                    .module
                    .declare_func_in_func(func_id, inner_builder.func)
            };

            let mut inner_emit = EmitContext::new(args.ctx.prefix.clone());
            inner_emit.lambda_counter = args.ctx.lambda_counter;
            inner_emit.current_fn = lambda_name.clone();
            if crate::debug::trace_level() >= crate::debug::TraceLevel::Calls {
                eprintln!(
                    "[emit-letrec] {} lam_binder={:#x}",
                    lambda_name, lam_binder.0
                );
            }
            inner_emit
                .env
                .insert(lam_binder, SsaVal::HeapPtr(inner_arg));

            // Load captures by position
            for (i, var_id) in sorted_fvs.iter().enumerate() {
                let offset = CLOSURE_CAPTURED_OFFSET + 8 * i as i32;
                let val =
                    inner_builder
                        .ins()
                        .load(types::I64, MemFlags::trusted(), inner_self, offset);
                inner_builder.declare_value_needs_stack_map(val);
                inner_emit.env.insert(*var_id, SsaVal::HeapPtr(val));
            }

            let body_root = lam_body_tree.nodes.len() - 1;
            let mut inner_sess = EmitSession {
                pipeline: args.sess.pipeline,
                vmctx: inner_vmctx,
                gc_sig: inner_gc_sig_ref,
                oom_func: inner_oom_func,
                tree: &lam_body_tree,
                lit_wrappers: args.sess.lit_wrappers,
            };
            let body_result = EmitContext::emit_node(
                EmitArgs {
                    ctx: &mut inner_emit,
                    sess: &mut inner_sess,
                    builder: &mut inner_builder,
                    tail: TailCtx::Tail,
                },
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

            args.ctx.lambda_counter = inner_emit.lambda_counter;

            args.sess
                .pipeline
                .define_function(lambda_func_id, &mut inner_ctx)?;

            let func_ref = args
                .sess
                .pipeline
                .module
                .declare_func_in_func(lambda_func_id, args.builder.func);
            let code_ptr = args.builder.ins().func_addr(types::I64, func_ref);
            args.builder.ins().store(
                MemFlags::trusted(),
                code_ptr,
                closure_ptr,
                CLOSURE_CODE_PTR_OFFSET,
            );

            // Zero-initialize capture slots so GC doesn't trace garbage
            let null_val = args.builder.ins().iconst(types::I64, 0);
            for i in 0..sorted_fvs.len() {
                let offset = CLOSURE_CAPTURED_OFFSET + 8 * i as i32;
                args.builder
                    .ins()
                    .store(MemFlags::trusted(), null_val, closure_ptr, offset);
            }

            // Fill captures already in env. Defer those referencing deferred simple bindings.
            for (i, var_id) in sorted_fvs.iter().enumerate() {
                let offset = CLOSURE_CAPTURED_OFFSET + 8 * i as i32;
                if let Some(ssaval) = args.ctx.env.get(var_id) {
                    let cap_val = ensure_heap_ptr(
                        args.builder,
                        args.sess.vmctx,
                        args.sess.gc_sig,
                        args.sess.oom_func,
                        *ssaval,
                    );
                    args.builder
                        .ins()
                        .store(MemFlags::trusted(), cap_val, closure_ptr, offset);
                } else {
                    pending_capture_updates
                        .entry(*var_id)
                        .or_default()
                        .push(ClosureCaptureSlot {
                            closure_ptr,
                            offset,
                        });
                }
            }
        }

        // Phase 3b: Fill Con fields that DON'T reference deferred simple bindings.
        let simple_binder_set: FxHashSet<VarId> = deferred_simple.iter().map(|(b, _)| *b).collect();
        let mut deferred_cons: Vec<(VarId, cranelift_codegen::ir::Value, Vec<usize>)> =
            Vec::with_capacity(rec_bindings.len());
        for pa in &pre_allocs {
            if let PreAlloc::Con {
                binder,
                ptr,
                field_indices,
            } = pa
            {
                let needs_simple = field_indices.iter().any(|&f_idx| {
                    matches!(&args.sess.tree.nodes[f_idx], CoreFrame::Var(v) if simple_binder_set.contains(v))
                });
                if needs_simple {
                    deferred_cons.push((*binder, *ptr, field_indices.clone()));
                } else {
                    for (i, &f_idx) in field_indices.iter().enumerate() {
                        let field_val = if is_trivial_field(f_idx, args.sess.tree) {
                            let val = emit_subtree(
                                EmitArgs {
                                    ctx: args.ctx,
                                    sess: args.sess,
                                    builder: args.builder,
                                    tail: TailCtx::NonTail,
                                },
                                f_idx,
                            )?;
                            ensure_heap_ptr(
                                args.builder,
                                args.sess.vmctx,
                                args.sess.gc_sig,
                                args.sess.oom_func,
                                val,
                            )
                        } else {
                            let thunk_val = emit_thunk(
                                EmitArgs {
                                    ctx: args.ctx,
                                    sess: args.sess,
                                    builder: args.builder,
                                    tail: TailCtx::NonTail,
                                },
                                f_idx,
                            )?;
                            thunk_val.value()
                        };
                        args.builder.ins().store(
                            MemFlags::trusted(),
                            field_val,
                            *ptr,
                            CON_FIELDS_OFFSET + 8 * i as i32,
                        );
                    }
                }
            }
        }

        // Topological sort for deferred simple bindings
        let deferred_simple = {
            let deferred_set: FxHashSet<VarId> = deferred_simple.iter().map(|(b, _)| *b).collect();

            let mut direct_deps: FxHashMap<VarId, Vec<VarId>> =
                FxHashMap::with_capacity_and_hasher(bindings.len(), Default::default());
            for (binder, rhs_idx) in bindings {
                let fvs =
                    tidepool_repr::free_vars::free_vars(&args.sess.tree.extract_subtree(*rhs_idx));
                direct_deps.insert(*binder, fvs.into_iter().collect());
            }

            let mut reachable_deferred: FxHashMap<VarId, FxHashSet<VarId>> =
                FxHashMap::with_capacity_and_hasher(deferred_simple.len(), Default::default());
            for &(start_node, _) in &deferred_simple {
                let mut visited = FxHashSet::default();
                let mut stack = vec![start_node];
                let mut reached = FxHashSet::default();

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
                    let blocked = reachable_deferred[&binder]
                        .iter()
                        .any(|fv| !sorted.iter().any(|(b, _): &(VarId, usize)| *b == *fv));
                    if blocked {
                        next_remaining.push((binder, rhs_idx));
                    } else {
                        sorted.push((binder, rhs_idx));
                        progress = true;
                    }
                }
                remaining = next_remaining;
            }
            sorted.extend(remaining);
            sorted
        };

        // Build deferred Con deps tracking
        let mut deferred_con_deps: Vec<DeferredConDep> = Vec::with_capacity(deferred_cons.len());
        for (_, ptr, field_indices) in &deferred_cons {
            let deps: FxHashSet<VarId> = field_indices
                .iter()
                .filter_map(|&f_idx| {
                    if let CoreFrame::Var(v) = &args.sess.tree.nodes[f_idx] {
                        if simple_binder_set.contains(v) {
                            return Some(*v);
                        }
                    }
                    None
                })
                .collect();
            deferred_con_deps.push(DeferredConDep {
                ptr: *ptr,
                field_indices: field_indices.clone(),
                remaining_deps: deps,
            });
        }

        // Store deferred state for LetRecSimpleEval/LetRecPostSimple/LetRecFinish
        let state_idx = args.ctx.push_letrec_state(LetRecDeferredState {
            pending_capture_updates,
            deferred_con_deps,
        });

        // Push work items in LIFO order: finish, then simple evals (reversed)
        work.push(EmitWork::LetRecFinish {
            body,
            state_idx,
            tail,
        });

        for (binder, rhs_idx) in deferred_simple.iter().rev() {
            if EmitContext::rhs_is_error_call_in_group(args.sess.tree, *rhs_idx, &group) {
                let poison_sv = emit_error_binding(
                    EmitArgs {
                        ctx: args.ctx,
                        sess: args.sess,
                        builder: args.builder,
                        tail: TailCtx::NonTail,
                    },
                    *rhs_idx,
                )?;
                if crate::debug::trace_level() >= crate::debug::TraceLevel::Scope {
                    args.ctx
                        .trace_scope(&format!("defer error LetRec(deferred) {:?}", binder));
                }
                args.ctx.env.insert(*binder, poison_sv);
                // Run post-step inline: closures may capture error-poisoned
                // bindings, and deferred Cons may depend on them. Without this,
                // capture slots stay zero-initialized \u2192 SIGSEGV instead of
                // clean poison closure invocation.
                Self::letrec_post_simple_step(
                    EmitArgs {
                        ctx: args.ctx,
                        sess: args.sess,
                        builder: args.builder,
                        tail: TailCtx::NonTail,
                    },
                    binder,
                    state_idx,
                )?;
            } else {
                let refs_deferred_con = !args
                    .ctx
                    .letrec_state(state_idx)
                    .deferred_con_deps
                    .is_empty()
                    && args
                        .ctx
                        .letrec_state(state_idx)
                        .deferred_con_deps
                        .iter()
                        .any(|d| d.remaining_deps.contains(binder));
                // Check if thunkification would drop sibling deps: emit_thunk
                // creates a fresh EmitContext and only captures vars in the
                // current env. Sibling deferred simple bindings not yet in env
                // would be dropped from captures \u2192 unresolved var at runtime.
                let can_thunkify = if refs_deferred_con {
                    let body_tree = args.sess.tree.extract_subtree(*rhs_idx);
                    let fvs = tidepool_repr::free_vars::free_vars(&body_tree);
                    !fvs.iter().any(|v| {
                        !args.ctx.env.contains_key(v) && deferred_simple.iter().any(|(b, _)| b == v)
                    })
                } else {
                    false
                };
                if can_thunkify {
                    // Thunked: compile as thunk inline (no work stack needed,
                    // emit_thunk creates a new EmitContext \u2014 bounded recursion).
                    let thunk_val = emit_thunk(
                        EmitArgs {
                            ctx: args.ctx,
                            sess: args.sess,
                            builder: args.builder,
                            tail: TailCtx::NonTail,
                        },
                        *rhs_idx,
                    )?;
                    if crate::debug::trace_level() >= crate::debug::TraceLevel::Scope {
                        args.ctx
                            .trace_scope(&format!("insert LetRec(simple) {:?}", binder));
                    }
                    args.ctx.env.insert(*binder, thunk_val);
                    Self::letrec_post_simple_step(
                        EmitArgs {
                            ctx: args.ctx,
                            sess: args.sess,
                            builder: args.builder,
                            tail: TailCtx::NonTail,
                        },
                        binder,
                        state_idx,
                    )?;
                } else {
                    // Non-thunked: push eval + post-step onto work stack
                    work.push(EmitWork::LetRecPostSimple {
                        binder: *binder,
                        state_idx,
                    });
                    work.push(EmitWork::Eval(*rhs_idx, TailCtx::NonTail));
                }
            }
        }

        Ok(())
    }

    /// Post-step after evaluating a deferred simple binding: fill pending
    /// captures and incrementally fill deferred Con fields.
    fn letrec_post_simple_step(
        args: EmitArgs,
        binder: &VarId,
        state_idx: LetRecStateId,
    ) -> Result<(), EmitError> {
        // Fill pending captures \u2014 take updates out to avoid borrowing self
        let updates = args
            .ctx
            .letrec_state_mut(state_idx)
            .pending_capture_updates
            .remove(binder);
        if let Some(updates) = updates {
            if let Some(ssaval) = args.ctx.env.get(binder) {
                let cap_val = ensure_heap_ptr(
                    args.builder,
                    args.sess.vmctx,
                    args.sess.gc_sig,
                    args.sess.oom_func,
                    *ssaval,
                );
                for slot in updates {
                    args.builder.ins().store(
                        MemFlags::trusted(),
                        cap_val,
                        slot.closure_ptr,
                        slot.offset,
                    );
                }
            }
        }

        // Incrementally fill deferred Cons whose deps are all satisfied.
        // Take out deferred_con_deps to avoid double-borrowing self
        // (emit_subtree/emit_thunk need &mut self).
        let mut con_deps =
            std::mem::take(&mut args.ctx.letrec_state_mut(state_idx).deferred_con_deps);
        for dep in con_deps.iter_mut() {
            dep.remaining_deps.remove(binder);
            if dep.remaining_deps.is_empty() && !dep.field_indices.is_empty() {
                for (i, &f_idx) in dep.field_indices.iter().enumerate() {
                    let field_val = if is_trivial_field(f_idx, args.sess.tree) {
                        let val = emit_subtree(
                            EmitArgs {
                                ctx: args.ctx,
                                sess: args.sess,
                                builder: args.builder,
                                tail: TailCtx::NonTail,
                            },
                            f_idx,
                        )?;
                        ensure_heap_ptr(
                            args.builder,
                            args.sess.vmctx,
                            args.sess.gc_sig,
                            args.sess.oom_func,
                            val,
                        )
                    } else {
                        let thunk_val = emit_thunk(
                            EmitArgs {
                                ctx: args.ctx,
                                sess: args.sess,
                                builder: args.builder,
                                tail: TailCtx::NonTail,
                            },
                            f_idx,
                        )?;
                        thunk_val.value()
                    };
                    args.builder.ins().store(
                        MemFlags::trusted(),
                        field_val,
                        dep.ptr,
                        CON_FIELDS_OFFSET + 8 * i as i32,
                    );
                }
                dep.field_indices.clear();
            }
        }
        args.ctx.letrec_state_mut(state_idx).deferred_con_deps = con_deps;

        Ok(())
    }

    /// LetRec phases 3a' and 3d: fill remaining captures and Con fields.
    fn letrec_finish_phases(args: EmitArgs, state_idx: LetRecStateId) -> Result<(), EmitError> {
        // Phase 3a': Fill any remaining closure capture slots.
        let pending =
            std::mem::take(&mut args.ctx.letrec_state_mut(state_idx).pending_capture_updates);
        for (var_id, updates) in pending {
            let ssaval = args.ctx.env.get(&var_id).ok_or_else(|| {
                EmitError::MissingCaptureVar(
                    var_id,
                    "LetRec Phase 3a' capture fill: not in env after Phase 3c".into(),
                )
            })?;
            let cap_val = ensure_heap_ptr(
                args.builder,
                args.sess.vmctx,
                args.sess.gc_sig,
                args.sess.oom_func,
                *ssaval,
            );
            for slot in updates {
                args.builder.ins().store(
                    MemFlags::trusted(),
                    cap_val,
                    slot.closure_ptr,
                    slot.offset,
                );
            }
        }

        // Phase 3d: Fill any deferred Con fields not already filled.
        let con_deps = std::mem::take(&mut args.ctx.letrec_state_mut(state_idx).deferred_con_deps);
        for dep in &con_deps {
            for (i, &f_idx) in dep.field_indices.iter().enumerate() {
                let field_val = if is_trivial_field(f_idx, args.sess.tree) {
                    let val = emit_subtree(
                        EmitArgs {
                            ctx: args.ctx,
                            sess: args.sess,
                            builder: args.builder,
                            tail: TailCtx::NonTail,
                        },
                        f_idx,
                    )?;
                    ensure_heap_ptr(
                        args.builder,
                        args.sess.vmctx,
                        args.sess.gc_sig,
                        args.sess.oom_func,
                        val,
                    )
                } else {
                    let thunk_val = emit_thunk(
                        EmitArgs {
                            ctx: args.ctx,
                            sess: args.sess,
                            builder: args.builder,
                            tail: TailCtx::NonTail,
                        },
                        f_idx,
                    )?;
                    thunk_val.value()
                };
                args.builder.ins().store(
                    MemFlags::trusted(),
                    field_val,
                    dep.ptr,
                    CON_FIELDS_OFFSET + 8 * i as i32,
                );
            }
        }

        Ok(())
    }

    fn push_letrec_state(&mut self, state: LetRecDeferredState) -> LetRecStateId {
        let idx = self.letrec_states.len();
        self.letrec_states.push(state);
        LetRecStateId(idx)
    }

    fn letrec_state(&self, id: LetRecStateId) -> &LetRecDeferredState {
        &self.letrec_states[id.0]
    }

    fn letrec_state_mut(&mut self, id: LetRecStateId) -> &mut LetRecDeferredState {
        &mut self.letrec_states[id.0]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LetRecStateId(usize);

/// Work items for the emit_node trampoline. Replaces recursive calls
/// with an explicit LIFO stack.
enum EmitWork {
    /// Evaluate node at tree index with given tail context \u2192 push result onto value stack
    Eval(usize, TailCtx),
    /// Pop value stack, bind to env
    Bind(VarId),
    /// After deferred simple binding eval: pop value, bind, fill captures + Cons
    LetRecPostSimple {
        binder: VarId,
        state_idx: LetRecStateId,
    },
    /// Phases 3a'/3d + push body eval
    LetRecFinish {
        body: usize,
        state_idx: LetRecStateId,
        tail: TailCtx,
    },
    /// Pop cleanup on return
    LetCleanupMark(LetCleanup),
}

/// Deferred state for LetRec phases 3c/3a'/3d, stored in EmitContext
/// so work items can reference it by index.
pub(crate) struct LetRecDeferredState {
    pending_capture_updates: FxHashMap<VarId, Vec<ClosureCaptureSlot>>,
    deferred_con_deps: Vec<DeferredConDep>,
}

pub(crate) struct ClosureCaptureSlot {
    pub closure_ptr: cranelift_codegen::ir::Value,
    pub offset: i32,
}

/// A pre-allocated Con whose field filling is deferred until its
/// simple-binding dependencies are satisfied.
struct DeferredConDep {
    ptr: cranelift_codegen::ir::Value,
    /// Field indices to fill. Emptied once filled (sentinel for "done").
    field_indices: Vec<usize>,
    /// Simple bindings this Con depends on. Entries removed as deps are satisfied.
    remaining_deps: FxHashSet<VarId>,
}

enum LetCleanup {
    Single(VarId, Option<SsaVal>),
    Rec(EnvScope),
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
        Literal::LitString(_) | Literal::LitByteArray(_) => {
            Err(EmitError::NotYetImplemented("LitString".into()))
        }
    }
}

/// Emit a `LitByteArray` (e.g. a `BigNat#` payload) as a heap Lit object.
///
/// Identical data-section layout to `emit_lit_string` (`[len: u64][bytes...]`),
/// but tagged `LIT_TAG_BYTEARRAY` instead of `LIT_TAG_STRING`. The tag matters at
/// read time: `unbox_bytearray` adds `+8` for STRING (to skip the length prefix,
/// for `unpackCString#`), but returns the data pointer as-is for BYTEARRAY — so
/// `sizeofByteArray#` reads the length and `indexWordArray#`/the mpn intercepts
/// (which add their own `+8`) read the limbs. Using STRING here would
/// double-offset and make `sizeofByteArray#` read a limb as the length.
fn emit_lit_bytearray_literal(
    pipeline: &mut CodegenPipeline,
    builder: &mut FunctionBuilder,
    vmctx: Value,
    gc_sig: ir::SigRef,
    oom_func: ir::FuncRef,
    bytes: &[u8],
    counter: &mut u32,
) -> Result<SsaVal, EmitError> {
    let data_name = format!("__litba_{}", *counter);
    *counter += 1;

    let data_id = pipeline
        .module
        .declare_data(&data_name, Linkage::Local, false, false)
        .map_err(|e| EmitError::CraneliftError(e.to_string()))?;

    let mut data_desc = DataDescription::new();
    data_desc.set_align(8);
    let mut contents = Vec::with_capacity(8 + bytes.len());
    contents.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    contents.extend_from_slice(bytes);
    data_desc.define(contents.into_boxed_slice());

    pipeline
        .module
        .define_data(data_id, &data_desc)
        .map_err(|e| EmitError::CraneliftError(e.to_string()))?;

    let local_data = pipeline.module.declare_data_in_func(data_id, builder.func);
    let data_ptr = builder.ins().symbol_value(types::I64, local_data);

    let ptr = emit_alloc_fast_path(builder, vmctx, LIT_TOTAL_SIZE, gc_sig, oom_func);
    let tag = builder.ins().iconst(types::I8, layout::TAG_LIT as i64);
    builder.ins().store(MemFlags::trusted(), tag, ptr, 0);
    let size = builder.ins().iconst(types::I16, LIT_TOTAL_SIZE as i64);
    builder.ins().store(MemFlags::trusted(), size, ptr, 1);
    let lit_tag = builder.ins().iconst(types::I8, LIT_TAG_BYTEARRAY);
    builder
        .ins()
        .store(MemFlags::trusted(), lit_tag, ptr, LIT_TAG_OFFSET);
    builder
        .ins()
        .store(MemFlags::trusted(), data_ptr, ptr, LIT_VALUE_OFFSET);
    builder.declare_value_needs_stack_map(ptr);
    Ok(SsaVal::HeapPtr(ptr))
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

/// Force a thunked SsaVal to WHNF. If the value is a HeapPtr pointing to a
/// TAG_THUNK object, emit code to call `heap_force` and return the result.
/// Raw values and non-thunk HeapPtrs pass through unchanged.
pub(crate) fn force_thunk_ssaval(
    pipeline: &mut CodegenPipeline,
    builder: &mut FunctionBuilder,
    vmctx: Value,
    val: SsaVal,
) -> Result<SsaVal, EmitError> {
    match val {
        SsaVal::Raw(_, _) => Ok(val),
        SsaVal::HeapPtr(ptr) => {
            let tag = builder.ins().load(types::I8, MemFlags::trusted(), ptr, 0);
            let is_thunk = builder
                .ins()
                .icmp_imm(IntCC::Equal, tag, layout::TAG_THUNK as i64);

            let force_block = builder.create_block();
            let ready_block = builder.create_block();
            builder.append_block_param(ready_block, types::I64);

            builder.ins().brif(
                is_thunk,
                force_block,
                &[],
                ready_block,
                &[BlockArg::Value(ptr)],
            );

            builder.switch_to_block(force_block);
            builder.seal_block(force_block);

            let force_fn = pipeline
                .module
                .declare_function("heap_force", Linkage::Import, &{
                    let mut sig = Signature::new(pipeline.isa.default_call_conv());
                    sig.params.push(AbiParam::new(types::I64)); // vmctx
                    sig.params.push(AbiParam::new(types::I64)); // thunk
                    sig.returns.push(AbiParam::new(types::I64)); // result
                    sig
                })
                .map_err(|e| EmitError::CraneliftError(e.to_string()))?;
            let force_ref = pipeline.module.declare_func_in_func(force_fn, builder.func);
            let call = builder.ins().call(force_ref, &[vmctx, ptr]);
            let forced = builder.inst_results(call)[0];
            builder.declare_value_needs_stack_map(forced);
            builder.ins().jump(ready_block, &[BlockArg::Value(forced)]);

            builder.switch_to_block(ready_block);
            builder.seal_block(ready_block);
            let result = builder.block_params(ready_block)[0];
            builder.declare_value_needs_stack_map(result);
            Ok(SsaVal::HeapPtr(result))
        }
    }
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
