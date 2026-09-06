//! Cranelift IR emission for Core expressions.
//!
//! Entry point: `compile_expr`. Emission is a stack-safe
//! hylomorphism over `EmitFrame` so deeply-nested Core can't overflow the host
//! stack.
//!
//! Tail-ness is owned by the `emit_node` spine, NOT carried through the hylo
//! (the #313 invariant): the hylo is hard-NonTail.

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
// GC-safe allocate-and-zero helper
// ---------------------------------------------------------------------------

/// Allocate a heap object, write its tag/size header, and zero-fill every
/// pointer-sized slot in `[fields_offset, fields_offset + n_slots*8)` — all in
/// one emit sequence with no GC point in between. Marks the returned pointer
/// as needing a stack map.
///
/// Every allocate-then-fill emit path (Con, Closure, Thunk) must go through
/// this so a GC triggered while filling real values into the (now zeroed)
/// slots never scans stale bump-heap bytes as pointers. Callers still write
/// any tag-specific metadata (con tag, field/capture count, thunk state, code
/// ptr) themselves — plain `iconst`+`store`, not GC points — before starting
/// whatever GC-triggering sub-emits fill the real slot values.
#[allow(clippy::too_many_arguments)]
fn emit_alloc_zeroed(
    builder: &mut FunctionBuilder,
    vmctx: Value,
    gc_sig: ir::SigRef,
    oom_func: ir::FuncRef,
    tag: u8,
    size: u64,
    fields_offset: i32,
    n_slots: usize,
) -> Value {
    let ptr = emit_alloc_fast_path(builder, vmctx, size, gc_sig, oom_func);

    let tag_val = builder.ins().iconst(types::I8, tag as i64);
    builder.ins().store(MemFlags::trusted(), tag_val, ptr, 0);
    let size_val = builder.ins().iconst(types::I32, size as i64);
    builder.ins().store(MemFlags::trusted(), size_val, ptr, 1);

    let null_val = builder.ins().iconst(types::I64, 0);
    for i in 0..n_slots {
        let offset = fields_offset + 8 * i as i32;
        builder
            .ins()
            .store(MemFlags::trusted(), null_val, ptr, offset);
    }

    builder.declare_value_needs_stack_map(ptr);
    ptr
}

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

    /// Error call sitting in App-ARGUMENT position. GHC's demand analysis can
    /// pass a bottoming fallback into a statically-dead arg slot (e.g. the
    /// `_last` fallback after `INLINE _Snoc` specialization). The JIT evaluates
    /// App arguments eagerly, so an eager `Raise` here would fire the dead
    /// error. Emit a LAZY poison closure instead (carrying the static message
    /// when known): it raises only if actually forced, matching Haskell's
    /// non-strict `error`. No children — the whole error-call subtree collapses
    /// to a constant poison-closure pointer.
    RaiseLazy {
        kind: u64,
        msg: Option<Vec<u8>>,
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
            EmitFrame::RaiseLazy { kind, msg } => EmitFrame::RaiseLazy { kind, msg },
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

/// Set of node indices that appear as the `arg` field of some `App` in the
/// tree, i.e. expressions evaluated in App-argument position. Used to route
/// error calls in argument position through a LAZY poison closure rather than
/// an eager `Raise` (see `EmitFrame::RaiseLazy`).
fn collect_app_arg_positions(tree: &CoreExpr) -> std::collections::HashSet<usize> {
    let mut set = std::collections::HashSet::new();
    for node in &tree.nodes {
        if let CoreFrame::App { arg, .. } = node {
            set.insert(*arg);
        }
    }
    set
}

/// Expand: classify a tree node into an EmitFrame.
fn expand_node(
    tree: &CoreExpr,
    idx: usize,
    arg_positions: &std::collections::HashSet<usize>,
) -> Result<EmitFrame<usize>, EmitError> {
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
                    let kind = EmitContext::extract_error_kind(tree, idx);
                    // Error call consumed as an App ARGUMENT: emit a LAZY poison
                    // closure (carrying the static message) instead of eagerly
                    // raising. GHC may pass a bottoming fallback into a
                    // statically-dead arg slot; raising it here is wrong because
                    // the JIT evaluates App args eagerly. The poison closure
                    // raises only if the slot is actually forced.
                    if arg_positions.contains(&idx) {
                        return Ok(EmitFrame::RaiseLazy {
                            kind,
                            msg: Some(msg),
                        });
                    }
                    // Static fast path: the message is known at compile time.
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

fn collapse_frame(args: EmitArgs, frame: EmitFrame<SsaVal>) -> Result<SsaVal, EmitError> {
    let tail = args.tail;
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
                // Session re-entry: a Var that misses the local env but is
                // seeded in the ExternalEnv resolves via its stable root slot
                // (see `SsaVal::from_external_slot`) — the GHCi-style
                // "reference a value bound in a prior fragment" path. Checked
                // FIRST, before the error-sentinel / unresolved-var-trap
                // handling: a session binder reaches codegen as an external
                // `NVar(stableVarId)` (0xFE-tagged), but the override is keyed
                // on ExternalEnv MEMBERSHIP, not on the tag — the tag is
                // incidental.
                if let Some(slot) = args.ctx.external_env.get(vid) {
                    return Ok(SsaVal::from_external_slot(args.builder, slot));
                }

                if let Some(sentinel) = vid.sentinel() {
                    // Lazy poison: emit a constant pointer to a pre-allocated
                    // poison closure. The error flag is NOT set now \u2014 only when
                    // the closure is actually called (forced). This is critical
                    // for typeclass dictionaries that contain error methods for
                    // impossible branches (e.g., $fFloatingDouble).
                    //
                    // An unresolved-external poison (kind 4) carries the slot of
                    // the symbol it replaced; resolving it against meta.cbor's
                    // `poisoned` table here is what lets the trap say
                    // "unresolved external Dep.helper" instead of a bare kind=4.
                    let kind = u64::from(sentinel.kind);
                    let poison_addr = match crate::host_fns::poisoned_external_name(sentinel.slot) {
                        Some(name) => {
                            crate::host_fns::error_poison_ptr_lazy_named(kind, &name) as i64
                        }
                        None => crate::host_fns::error_poison_ptr_lazy(kind) as i64,
                    };
                    let poison_val = args.builder.ins().iconst(types::I64, poison_addr);
                    return Ok(SsaVal::HeapPtr(poison_val));
                }

                args.ctx.trace_scope(&format!(
                    "MISS var {:?} (env has {} entries)",
                    vid,
                    args.ctx.env.len()
                ));
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
            let size_val = args.builder.ins().iconst(types::I32, size as i64);
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
            let num_fields = field_indices.len();
            let size = 24 + 8 * num_fields as u64;
            let ptr = emit_alloc_zeroed(
                args.builder,
                args.sess.vmctx,
                args.sess.gc_sig,
                args.sess.oom_func,
                layout::TAG_CON,
                size,
                CON_FIELDS_OFFSET,
                num_fields,
            );

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

            Ok(SsaVal::HeapPtr(ptr))
        }
        EmitFrame::PrimOp {
            ref op,
            args: ref prim_args,
        } => {
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
            let raw_fun_ptr = fun.value();
            let arg_ptr = ensure_heap_ptr(
                args.builder,
                args.sess.vmctx,
                args.sess.gc_sig,
                args.sess.oom_func,
                arg,
            );
            crate::emit::apply::runtime_apply(args.sess, args.builder, raw_fun_ptr, arg_ptr)
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
            let join_info = args.ctx.join_blocks.get(&label)?;
            let join_block = join_info.block;
            let recursive = join_info.recursive;

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

            // External-cancellation safepoint for recursive join back-edges
            // (#325) — the one loop shape the trampoline / gc_trigger / effect
            // safepoints all miss. No-op for forward (non-recursive) joins.
            crate::emit::join::emit_join_cancel_safepoint(
                args.sess.pipeline,
                args.builder,
                args.sess.vmctx,
                recursive,
            )?;

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
        EmitFrame::RaiseLazy { kind, msg } => {
            // Constant pointer to a pre-allocated lazy poison closure. The error
            // flag is set only when the closure is forced (called / pattern
            // matched), so a statically-dead arg slot never raises. Preserves
            // the static message via the message-carrying poison variant when
            // known.
            let poison_addr = match &msg {
                Some(bytes) => crate::host_fns::error_poison_ptr_lazy_msg(kind, bytes) as i64,
                None => crate::host_fns::error_poison_ptr_lazy(kind) as i64,
            };
            let poison_val = args.builder.ins().iconst(types::I64, poison_addr);
            Ok(SsaVal::HeapPtr(poison_val))
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
    let arg_positions = collect_app_arg_positions(args.sess.tree);
    try_expand_and_collapse::<EmitFrameToken, _, _, _>(
        idx,
        |idx| expand_node(args.sess.tree, idx, &arg_positions),
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
///
/// Shared with the oracle (`tidepool-eval`) via `tidepool_repr::trivial_field`
/// — both backends MUST agree on this predicate (see that module's doc for
/// why: a diverged copy is a real oracle/JIT semantic disagreement, not just
/// duplicated code).
use tidepool_repr::trivial_field::is_trivial_field;

/// Topologically sort deferred simple LetRec bindings so each appears AFTER the
/// deferred-simple siblings it (transitively) depends on. `all_bindings` is the
/// full Rec group (the dependency graph spans Lam/Con siblings too). A true
/// cycle leaves its members un-orderable; they are appended last (resolving to
/// unresolved-on-force — the `cycle` Known-Limit). Binding in this order lets
/// each thunk capture its already-bound deps instead of dropping them.
fn topo_sort_deferred_simple(
    deferred_simple: Vec<(VarId, usize)>,
    all_bindings: &[(VarId, usize)],
    free_vars_idx: &tidepool_repr::free_vars::FreeVarsIndex,
) -> Vec<(VarId, usize)> {
    use petgraph::graph::{DiGraph, NodeIndex};
    use petgraph::visit::{Dfs, EdgeRef};
    use petgraph::Direction;
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;

    let deferred_set: FxHashSet<VarId> = deferred_simple.iter().map(|(b, _)| *b).collect();

    // Full dependency graph over EVERY binding in the Rec group (Lam/Con
    // siblings included — a deferred-simple binding can depend on one
    // transitively, e.g. through an intermediate Lam), nodes inserted in
    // `all_bindings` order so the structure is deterministic. Edge
    // binder -> dep means "binder's rhs references dep"; a self-reference
    // (dep == binder, a corecursive value knot) is deliberately never
    // edged — the value-knot mechanism (promised captures) handles that
    // case, not binding order, so it must never count as a blocking dep.
    let mut node_of: FxHashMap<VarId, NodeIndex> =
        FxHashMap::with_capacity_and_hasher(all_bindings.len(), Default::default());
    let mut full_graph: DiGraph<VarId, ()> = DiGraph::with_capacity(all_bindings.len(), 0);
    for (binder, _) in all_bindings {
        node_of.insert(*binder, full_graph.add_node(*binder));
    }
    for (binder, rhs_idx) in all_bindings {
        let binder_node = node_of[binder];
        for dep in free_vars_idx.free_vars_at(*rhs_idx) {
            if dep == *binder {
                continue;
            }
            if let Some(&dep_node) = node_of.get(&dep) {
                full_graph.add_edge(binder_node, dep_node, ());
            }
        }
    }

    // For each deferred-simple binding, the set of OTHER deferred-simple
    // bindings it transitively depends on (a DFS over `full_graph` following
    // outgoing "depends on" edges, restricted to hits in `deferred_set`,
    // excluding the start node itself even if a cycle routes back to it).
    let mut reachable_deferred: FxHashMap<VarId, FxHashSet<VarId>> =
        FxHashMap::with_capacity_and_hasher(deferred_simple.len(), Default::default());
    for &(start, _) in &deferred_simple {
        let start_node = node_of[&start];
        let mut dfs = Dfs::new(&full_graph, start_node);
        let mut reached = FxHashSet::default();
        while let Some(nx) = dfs.next(&full_graph) {
            if nx == start_node {
                continue;
            }
            let v = full_graph[nx];
            if deferred_set.contains(&v) {
                reached.insert(v);
            }
        }
        reachable_deferred.insert(start, reached);
    }

    // Ordering graph over ONLY the deferred-simple bindings, nodes inserted
    // in `deferred_simple`'s original order (so `NodeIndex` order IS original
    // input order — the tie-break the min-heap below relies on). Edge
    // dep -> dependent: dep must be emitted before dependent.
    let mut order_node_of: FxHashMap<VarId, NodeIndex> =
        FxHashMap::with_capacity_and_hasher(deferred_simple.len(), Default::default());
    let mut order_graph: DiGraph<usize, ()> = DiGraph::with_capacity(deferred_simple.len(), 0);
    for (i, (binder, _)) in deferred_simple.iter().enumerate() {
        order_node_of.insert(*binder, order_graph.add_node(i));
    }
    for (binder, _) in &deferred_simple {
        let dependent_node = order_node_of[binder];
        for dep in &reachable_deferred[binder] {
            if let Some(&dep_node) = order_node_of.get(dep) {
                order_graph.add_edge(dep_node, dependent_node, ());
            }
        }
    }

    // Kahn's algorithm: a min-heap over `NodeIndex` breaks ties among
    // simultaneously-ready nodes by earliest original position (`NodeIndex`
    // order == original `deferred_simple` order, by construction above),
    // giving left-to-right, resolve-as-soon-as-ready ordering (see the
    // golden tests pinning chain/diamond/independent/cycle/self-reference
    // shapes).
    let n = order_graph.node_count();
    let mut in_degree = vec![0usize; n];
    for nx in order_graph.node_indices() {
        in_degree[nx.index()] = order_graph.edges_directed(nx, Direction::Incoming).count();
    }
    let mut ready: BinaryHeap<Reverse<NodeIndex>> = order_graph
        .node_indices()
        .filter(|nx| in_degree[nx.index()] == 0)
        .map(Reverse)
        .collect();
    let mut resolved = vec![false; n];
    let mut order: Vec<NodeIndex> = Vec::with_capacity(n);
    while let Some(Reverse(nx)) = ready.pop() {
        resolved[nx.index()] = true;
        order.push(nx);
        for edge in order_graph.edges_directed(nx, Direction::Outgoing) {
            let target = edge.target();
            in_degree[target.index()] -= 1;
            if in_degree[target.index()] == 0 {
                ready.push(Reverse(target));
            }
        }
    }
    // A true cycle leaves its members permanently at in-degree > 0; append
    // them last, in original relative order (never resolved to `false`, and
    // `node_indices()` walks in insertion == original order) — the `cycle`
    // Known-Limit fallback.
    for nx in order_graph.node_indices() {
        if !resolved[nx.index()] {
            order.push(nx);
        }
    }

    order
        .into_iter()
        .map(|nx| deferred_simple[order_graph[nx]])
        .collect()
}

// ---------------------------------------------------------------------------
// Lam compilation helper (extracted for readability)
// ---------------------------------------------------------------------------

/// Compute sorted capture list for a closure/thunk body.
/// If `exclude` is Some, that VarId is removed from free vars (for lambda binders).
fn compute_captures(
    ctx: &EmitContext,
    tree: &CoreExpr,
    free_vars_idx: &tidepool_repr::free_vars::FreeVarsIndex,
    body_idx: usize,
    exclude: Option<VarId>,
    label: &str,
) -> (CoreExpr, Vec<VarId>) {
    compute_captures_promised(ctx, tree, free_vars_idx, body_idx, exclude, label, None)
}

/// [`compute_captures`] with a `promised` set: free vars in it are KEPT in the
/// capture list even though they are not (yet) in env — the caller emits their
/// slots as placeholders and patches them once the awaited binder lands (the
/// LetRec value-knot case; see the Phase 3c knot-tying comment).
fn compute_captures_promised(
    ctx: &EmitContext,
    tree: &CoreExpr,
    free_vars_idx: &tidepool_repr::free_vars::FreeVarsIndex,
    body_idx: usize,
    exclude: Option<VarId>,
    label: &str,
    promised: Option<&FxHashSet<VarId>>,
) -> (CoreExpr, Vec<VarId>) {
    // `free_vars_idx` is built from `tree` (see `EmitSession::free_vars_idx`'s
    // doc), so querying it at `body_idx` is equivalent to computing free vars
    // on the extracted `body_tree` directly, without a second walk.
    // `extract_subtree` still runs regardless: `body_tree` becomes the nested
    // Lam/Thunk's own EmitSession tree, not just a free-vars scratch value.
    let body_tree = tree.extract_subtree(body_idx);
    let fvs = free_vars_idx.free_vars_at(body_idx);
    let keep = |v: &VarId| ctx.env.contains_key(v) || promised.is_some_and(|p| p.contains(v));

    let dropped: Vec<VarId> = fvs.iter().filter(|v| !keep(v)).copied().collect();
    if !dropped.is_empty() {
        ctx.trace_scope(&format!(
            "{} capture: dropped {} free vars not in scope: {:?}",
            label,
            dropped.len(),
            dropped
        ));
    }
    let mut sorted_fvs: Vec<VarId> = fvs.into_iter().filter(|v| keep(v)).collect();

    if let Some(binder) = exclude {
        if let Ok(idx) = sorted_fvs.binary_search(&binder) {
            sorted_fvs.remove(idx);
        }
    }
    (body_tree, sorted_fvs)
}

/// What varies between the three nested-function compilation sites
/// ([`emit_lam`], [`emit_thunk_promised`], and LetRec phase 3a): arity,
/// capture-slot stride, and tail position. Everything else — signature
/// shape, function declaration, the inner `FunctionBuilder`/block/stack-map
/// setup, the `runtime_oom` import, capture loading, body emission, and the
/// `ensure_heap_ptr`'d return — is identical and lives in
/// [`compile_nested_body`].
struct NestedFnSpec<'a> {
    /// Already minted via `next_lambda_name()`/`next_thunk_name()` — naming
    /// policy (and which counter it draws from) stays with the caller.
    name: String,
    /// `Some(binder)` gives the function a third `arg` parameter, bound to
    /// `binder` in the body's env — a Lam (ordinary or LetRec-recursive).
    /// `None` omits the parameter entirely — a Thunk, which takes no
    /// argument.
    arg_binder: Option<VarId>,
    /// Byte offset of the first capture slot in `self`
    /// (`CLOSURE_CAPTURED_OFFSET` or `THUNK_CAPTURED_OFFSET`) — every
    /// capture site uses the same 8-byte stride.
    captured_offset: i32,
    /// Captures to load from `self`, in slot order: index `i` loads from
    /// `captured_offset + 8*i` and binds it to `capture_vars[i]`.
    capture_vars: &'a [VarId],
    /// The already-extracted, standalone body tree (`compute_captures`/
    /// `compute_captures_promised` already ran; this is their `body_tree`).
    body_tree: &'a CoreExpr,
    tail: TailCtx,
}

/// Compile `spec` as a fresh Cranelift function — `(vmctx, self[, arg]) -> i64`
/// — and return the CODE POINTER as an outer-function `Value` (via
/// `declare_func_in_func` + `func_addr`), ready for the caller to store into
/// a closure/thunk object. Declaration, allocation, and capture-slot FILLING
/// stay with the caller: a Lam allocates its own closure and already has
/// every capture value in hand; a Thunk allocates its own thunk object and
/// may leave some captures as `promised` null placeholders; LetRec phase 3a
/// fills a closure Phase 1 already pre-allocated and may defer some captures
/// to `pending_capture_updates`. Those three shapes are real, not
/// accidental duplication — only the function-compilation machinery below
/// was.
fn compile_nested_body(args: &mut EmitArgs, spec: NestedFnSpec) -> Result<Value, EmitError> {
    let mut sig = Signature::new(args.sess.pipeline.isa.default_call_conv());
    sig.params.push(AbiParam::new(types::I64)); // vmctx
    sig.params.push(AbiParam::new(types::I64)); // self
    if spec.arg_binder.is_some() {
        sig.params.push(AbiParam::new(types::I64)); // arg
    }
    sig.returns.push(AbiParam::new(types::I64));

    let func_id = args
        .sess
        .pipeline
        .module
        .declare_function(&spec.name, Linkage::Local, &sig)
        .map_err(|e| EmitError::CraneliftError(e.to_string()))?;
    args.sess
        .pipeline
        .register_lambda(func_id, spec.name.clone());
    if let Some(binder) = spec.arg_binder {
        log::trace!(target: "tidepool::calls", "[emit] {} binder={:#x}", spec.name, binder.0);
    }

    let mut inner_ctx = Context::new();
    inner_ctx.func.signature = sig;
    inner_ctx.func.name = UserFuncName::default();

    let mut inner_fb_ctx = FunctionBuilderContext::new();
    let mut inner_builder = FunctionBuilder::new(&mut inner_ctx.func, &mut inner_fb_ctx);
    let inner_block = inner_builder.create_block();
    inner_builder.append_block_params_for_function_params(inner_block);
    inner_builder.switch_to_block(inner_block);
    inner_builder.seal_block(inner_block);

    let inner_vmctx = inner_builder.block_params(inner_block)[0];
    let inner_self = inner_builder.block_params(inner_block)[1];
    let inner_arg = spec
        .arg_binder
        .map(|_| inner_builder.block_params(inner_block)[2]);

    inner_builder.declare_value_needs_stack_map(inner_self);
    if let Some(arg_val) = inner_arg {
        inner_builder.declare_value_needs_stack_map(arg_val);
    }

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
    // Propagate session bindings into the nested function's own context so a
    // Var-miss inside the body can resolve them. Empty in the one-shot path.
    // See the `external_env` per-function-Value invariant.
    inner_emit.external_env = args.ctx.external_env.clone();
    inner_emit.lambda_counter = args.ctx.lambda_counter;
    inner_emit.current_fn = spec.name.clone();

    if let (Some(binder), Some(arg_val)) = (spec.arg_binder, inner_arg) {
        inner_emit.trace_scope(&format!("insert lam binder {:?}", binder));
        inner_emit.env.insert(binder, SsaVal::HeapPtr(arg_val));
    }

    for (i, var_id) in spec.capture_vars.iter().enumerate() {
        let offset = spec.captured_offset + 8 * i as i32;
        let val = inner_builder
            .ins()
            .load(types::I64, MemFlags::trusted(), inner_self, offset);
        inner_builder.declare_value_needs_stack_map(val);
        inner_emit.trace_scope(&format!("insert capture {:?}", var_id));
        inner_emit.env.insert(*var_id, SsaVal::HeapPtr(val));
    }

    let body_root = spec.body_tree.nodes.len() - 1;
    let mut inner_sess = EmitSession {
        pipeline: args.sess.pipeline,
        vmctx: inner_vmctx,
        gc_sig: inner_gc_sig_ref,
        oom_func: inner_oom_func,
        tree: spec.body_tree,
        lit_wrappers: args.sess.lit_wrappers,
        free_vars_idx: tidepool_repr::free_vars::FreeVarsIndex::compute(spec.body_tree),
        function_imports: FunctionImports::default(),
    };
    let body_result = EmitContext::emit_node(
        EmitArgs {
            ctx: &mut inner_emit,
            sess: &mut inner_sess,
            builder: &mut inner_builder,
            tail: spec.tail,
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
        .define_function(func_id, &mut inner_ctx)?;

    let func_ref = args
        .sess
        .pipeline
        .module
        .declare_func_in_func(func_id, args.builder.func);
    Ok(args.builder.ins().func_addr(types::I64, func_ref))
}

fn emit_lam(mut args: EmitArgs, binder: VarId, body_idx: usize) -> Result<SsaVal, EmitError> {
    let (body_tree, sorted_fvs) = compute_captures(
        args.ctx,
        args.sess.tree,
        &args.sess.free_vars_idx,
        body_idx,
        Some(binder),
        "lam",
    );

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
    let capture_vars: Vec<VarId> = captures.iter().map(|(v, _)| *v).collect();

    let lambda_name = args.ctx.next_lambda_name();
    let code_ptr = compile_nested_body(
        &mut args,
        NestedFnSpec {
            name: lambda_name,
            arg_binder: Some(binder),
            captured_offset: CLOSURE_CAPTURED_OFFSET,
            capture_vars: &capture_vars,
            body_tree: &body_tree,
            tail: TailCtx::Tail,
        },
    )?;

    let num_captures = captures.len();
    let closure_size = 24 + 8 * num_captures as u64;
    let closure_ptr = emit_alloc_zeroed(
        args.builder,
        args.sess.vmctx,
        args.sess.gc_sig,
        args.sess.oom_func,
        layout::TAG_CLOSURE,
        closure_size,
        CLOSURE_CAPTURED_OFFSET,
        num_captures,
    );

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

    // Capture slots are already zeroed (emit_alloc_zeroed above), so a GC
    // triggered by `ensure_heap_ptr` mid-loop (e.g. a Raw capture forcing a
    // Lit allocation) never scans an unfilled slot as a stale pointer.
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
    Ok(emit_thunk_promised(args, body_idx, None)?.0)
}

/// [`emit_thunk`] with a `promised` capture set for LetRec value knots: each
/// promised var (a letrec binder not yet in env — self or a later sibling)
/// gets a real capture SLOT holding a null placeholder, and its
/// `(var, slot offset)` is returned so the caller can register it in
/// `pending_capture_updates` for patching when the binder lands. Null is safe
/// meanwhile: the GC's slot walker skips null (`!ptr.is_null()` guard), and
/// nothing can force the thunk before the letrec completes.
fn emit_thunk_promised(
    mut args: EmitArgs,
    body_idx: usize,
    promised: Option<&FxHashSet<VarId>>,
) -> Result<(SsaVal, Vec<(VarId, i32)>), EmitError> {
    let (body_tree, sorted_fvs) = compute_captures_promised(
        args.ctx,
        args.sess.tree,
        &args.sess.free_vars_idx,
        body_idx,
        None,
        "thunk",
        promised,
    );

    let captures: Vec<(VarId, Option<SsaVal>)> = sorted_fvs
        .iter()
        .map(|v| match args.ctx.env.get(v) {
            Some(val) => Ok((*v, Some(*val))),
            None if promised.is_some_and(|p| p.contains(v)) => Ok((*v, None)),
            None => Err(EmitError::MissingCaptureVar(
                *v,
                format!(
                    "Thunk capture: not in env (env has {} vars)",
                    args.ctx.env.len()
                ),
            )),
        })
        .collect::<Result<Vec<_>, EmitError>>()?;

    let thunk_name = args.ctx.next_thunk_name();
    let code_ptr = compile_nested_body(
        &mut args,
        NestedFnSpec {
            name: thunk_name,
            arg_binder: None,
            captured_offset: THUNK_CAPTURED_OFFSET,
            capture_vars: &sorted_fvs,
            body_tree: &body_tree,
            tail: TailCtx::NonTail,
        },
    )?;

    // Allocate the thunk heap object, capture slots pre-zeroed so a GC
    // triggered by `ensure_heap_ptr` mid-loop below never scans an unfilled
    // slot as a stale pointer.
    let num_captures = captures.len();
    let thunk_size = 24 + 8 * num_captures as u64;
    let thunk_ptr = emit_alloc_zeroed(
        args.builder,
        args.sess.vmctx,
        args.sess.gc_sig,
        args.sess.oom_func,
        layout::TAG_THUNK,
        thunk_size,
        THUNK_CAPTURED_OFFSET,
        num_captures,
    );

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

    let mut pending_slots: Vec<(VarId, i32)> = Vec::new();
    for (i, (var_id, maybe_val)) in captures.iter().enumerate() {
        let offset = THUNK_CAPTURED_OFFSET + 8 * i as i32;
        match maybe_val {
            Some(ssaval) => {
                let cap_val = ensure_heap_ptr(
                    args.builder,
                    args.sess.vmctx,
                    args.sess.gc_sig,
                    args.sess.oom_func,
                    *ssaval,
                );
                args.builder
                    .ins()
                    .store(MemFlags::trusted(), cap_val, thunk_ptr, offset);
            }
            None => {
                let null = args.builder.ins().iconst(types::I64, 0);
                args.builder
                    .ins()
                    .store(MemFlags::trusted(), null, thunk_ptr, offset);
                pending_slots.push((*var_id, offset));
            }
        }
    }

    Ok((SsaVal::HeapPtr(thunk_ptr), pending_slots))
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
    external_env: &ExternalEnv,
) -> Result<FuncId, EmitError> {
    // Built once, up front, for the whole compilation: this compile_expr call
    // is one `EmitSession::tree` scope end to end (nested Lam/Thunk bodies
    // get their OWN fresh index over their own extracted tree — see
    // `EmitSession::free_vars_idx`'s doc), so the top-level `EmitSession`
    // constructed further down can reuse this one analysis rather than
    // re-deriving it.
    let free_vars_idx = tidepool_repr::free_vars::FreeVarsIndex::compute(tree);

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
    // Seed session-scoped external bindings for Var-miss resolution. Empty
    // for the one-shot path; cloned into nested function contexts below.
    emit_ctx.external_env = external_env.clone();

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
        free_vars_idx,
        function_imports: FunctionImports::default(),
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
    /// GHC Core hoists `error "..."` into let bindings that are only forced on
    /// impossible branches; since the JIT is strict, such bindings must not be
    /// evaluated eagerly. True when the RHS at `rhs_idx` is a direct error
    /// call: a bare error Var, an App chain whose head function is an error
    /// Var, or (for an unlifted-type CAF) a case whose scrutinee bottoms. More
    /// precise than a free-vars scan, which would poison any binding that
    /// CONTAINED an error reference anywhere (e.g. in a case branch fallback)
    /// even when the main path is valid.
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
                // fallback: UserError (shared ABI discriminant, not a magic 2)
                _ => return crate::host_fns::RuntimeErrorKind::UserError as u64,
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
                    } else {
                        // A genuinely free/dynamic variable reachable in the
                        // message subtree (not resolvable to a literal-producing
                        // let-binding) means this is NOT a pure-literal message
                        // — e.g. `error ("prefix" <> dynamicVar)`. Abort the
                        // whole search rather than silently skipping it, so the
                        // caller falls through to the dynamic
                        // (runtime_error_dynamic/materialize_message) path
                        // instead of truncating the message to a leading
                        // literal fragment.
                        return None;
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
        // The production/proptest path already runs emit on a large worker
        // stack; this `maybe_grow` is the cheap guarantee for any path where
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
                                let binder = *binder;
                                let rhs = *rhs;
                                let body = *body;
                                // Dead code elimination: skip RHS if binder is unused in body.
                                // The extracted subtree is scoped to the walk so it is
                                // freed before the branches below re-enter emission —
                                // an emit_thunk recursion holding one clone per level
                                // would otherwise stack them up.
                                let body_fvs = {
                                    let body_subtree = args.sess.tree.extract_subtree(body);
                                    tidepool_repr::free_vars::free_vars(&body_subtree)
                                };
                                if body_fvs.binary_search(&binder).is_ok() {
                                    if is_trivial_field(rhs, args.sess.tree) {
                                        // Trivial RHS (already WHNF \u2014 Var/Lit/Lam/Con \u2014 or a
                                        // strict, terminating PrimOp): evaluate eagerly. This is
                                        // the fast path; no thunk allocation.
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
                                    } else {
                                        // Non-trivial RHS (App/Case/Let/Jump): thunkify. GHC Core
                                        // `let` is NON-STRICT \u2014 strictness is expressed via `case`,
                                        // never `let`. Eager eval here would force a productive
                                        // corecursion into infinite self-recursion: ReadP `expect`
                                        // is `F = \k -> let x = F k in <Get parser using x>`, where
                                        // the `let x = F k` is the lazy "next layer"; the strict
                                        // spine drove it to StackOverflow while eval (lazy) is
                                        // bounded by demand. Binding a thunk restores the correct
                                        // semantics \u2014 and subsumes the error-deferral special case
                                        // above (a bottoming RHS is just one non-terminating RHS).
                                        let thunk_val = emit_thunk(
                                            EmitArgs {
                                                ctx: args.ctx,
                                                sess: args.sess,
                                                builder: args.builder,
                                                tail: TailCtx::NonTail,
                                            },
                                            rhs,
                                        )?;
                                        let old_val = args.ctx.env.insert(binder, thunk_val);
                                        // No work-stack RHS eval; push cleanup, continue to body.
                                        work.push(EmitWork::LetCleanupMark(LetCleanup::Single(
                                            binder, old_val,
                                        )));
                                    }
                                } else {
                                    args.ctx
                                        .trace_scope(&format!("DCE skip LetNonRec {:?}", binder));
                                }
                                idx = body;
                                continue;
                            }
                            CoreFrame::LetRec { bindings, body } => {
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
                    args.ctx
                        .trace_scope(&format!("insert LetNonRec {:?}", binder));
                    args.ctx.env.insert(binder, val);
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
                        args.ctx
                            .trace_scope(&format!("restore LetCleanup {:?}", var));
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
        crate::emit::apply::runtime_tail_apply(args.sess, args.builder, raw_fun_ptr, arg_ptr)
    }

    /// Execute LetRec phases 1-3b inline, then push deferred-simple evals
    /// (phase 3c) and finish (3a'/3d) onto the work stack.
    fn emit_letrec_phases(
        mut args: EmitArgs,
        bindings: &[(VarId, usize)],
        body: usize,
        work: &mut Vec<EmitWork>,
    ) -> Result<(), EmitError> {
        let tail = args.tail;
        // Split bindings: Lam/Con need 3-phase pre-allocation (recursive),
        // everything else binds lazily as simple bindings (Phase 3c).
        let (rec_bindings, simple_bindings): (Vec<_>, Vec<_>) =
            bindings.iter().partition(|(_, rhs_idx)| {
                matches!(
                    &args.sess.tree.nodes[*rhs_idx],
                    CoreFrame::Lam { .. } | CoreFrame::Con { .. }
                )
            });

        // All-simple Rec (no Lam/Con): no closures/cyclic data to knot-tie, so
        // there is no pre-alloc and no incremental fill. Classify exactly like the
        // rec-present path — error-call RHS → lazy poison; everything else binds
        // in topological order under the lazy-default rule (thunk unless the RHS
        // is trivially resolvable now). The LetRecFinish then evaluates the body.
        if rec_bindings.is_empty() {
            let state_idx = args.ctx.push_letrec_state(LetRecDeferredState {
                pending_capture_updates: FxHashMap::default(),
                deferred_con_deps: Vec::new(),
            });
            work.push(EmitWork::LetRecFinish {
                body,
                state_idx,
                tail,
            });

            let deferred_simple: Vec<(VarId, usize)> =
                simple_bindings.iter().map(|(b, r)| (*b, *r)).collect();
            let deferred_simple =
                topo_sort_deferred_simple(deferred_simple, bindings, &args.sess.free_vars_idx);
            let letrec_binders: FxHashSet<VarId> = bindings.iter().map(|(b, _)| *b).collect();
            for (binder, rhs_idx) in deferred_simple.iter() {
                let fvs = args.sess.free_vars_idx.free_vars_at(*rhs_idx);
                let resolvable_now = is_trivial_field(*rhs_idx, args.sess.tree)
                    && fvs.iter().all(|v| args.ctx.env.contains_key(v));
                let sv = if resolvable_now {
                    emit_subtree(
                        EmitArgs {
                            ctx: args.ctx,
                            sess: args.sess,
                            builder: args.builder,
                            tail: TailCtx::NonTail,
                        },
                        *rhs_idx,
                    )?
                } else {
                    // Value-knot support (see the rec-present Phase 3c twin):
                    // cyclic simple bindings capture not-yet-bound letrec
                    // binders via null placeholder slots, patched by
                    // letrec_post_simple_step below.
                    let promised: FxHashSet<VarId> = fvs
                        .iter()
                        .filter(|v| !args.ctx.env.contains_key(v) && letrec_binders.contains(v))
                        .copied()
                        .collect();
                    let (sv, pending) = emit_thunk_promised(
                        EmitArgs {
                            ctx: args.ctx,
                            sess: args.sess,
                            builder: args.builder,
                            tail: TailCtx::NonTail,
                        },
                        *rhs_idx,
                        if promised.is_empty() {
                            None
                        } else {
                            Some(&promised)
                        },
                    )?;
                    if !pending.is_empty() {
                        let SsaVal::HeapPtr(thunk_ptr) = sv else {
                            unreachable!("emit_thunk_promised returns a heap ptr")
                        };
                        let state = args.ctx.letrec_state_mut(state_idx);
                        for (awaited, offset) in pending {
                            state
                                .pending_capture_updates
                                .entry(awaited)
                                .or_default()
                                .push(ClosureCaptureSlot {
                                    closure_ptr: thunk_ptr,
                                    offset,
                                });
                        }
                    }
                    sv
                };
                args.ctx.env.insert(*binder, sv);
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
                    // Only `fvs` is needed here (Phase 1 sizes the closure and
                    // records the capture list); the extracted subtree itself
                    // isn't kept — Phase 3a (below) re-extracts `lam_body` on
                    // its own when it actually needs a standalone tree to
                    // compile the lambda body against.
                    let mut fvs = args.sess.free_vars_idx.free_vars_at(*lam_body);
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
                    if !dropped_fvs.is_empty() {
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
                    // Capture slots pre-zeroed: the NEXT binding's pre-alloc
                    // (or any later GC point before Phase 3a fills them) must
                    // never see stale bump-heap bytes as pointers.
                    let closure_ptr = emit_alloc_zeroed(
                        args.builder,
                        args.sess.vmctx,
                        args.sess.gc_sig,
                        args.sess.oom_func,
                        layout::TAG_CLOSURE,
                        closure_size,
                        CLOSURE_CAPTURED_OFFSET,
                        num_captures,
                    );

                    let num_cap_val = args.builder.ins().iconst(types::I16, num_captures as i64);
                    args.builder.ins().store(
                        MemFlags::trusted(),
                        num_cap_val,
                        closure_ptr,
                        CLOSURE_NUM_CAPTURED_OFFSET,
                    );

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
                    let ptr = emit_alloc_zeroed(
                        args.builder,
                        args.sess.vmctx,
                        args.sess.gc_sig,
                        args.sess.oom_func,
                        layout::TAG_CON,
                        size,
                        CON_FIELDS_OFFSET,
                        num_fields,
                    );

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
            args.ctx
                .trace_scope(&format!("insert LetRec(rec) {:?}", binder));
            args.ctx.env.insert(binder, SsaVal::HeapPtr(ptr));
        }

        // Phase 2.5: collect simple bindings. The topo-sorted Phase 3c loop binds
        // each lazily (a thunk) — or eagerly when it is trivially resolvable now
        // (a Var alias is trivially resolvable, so it takes this eager path
        // too). An error-call RHS (`error …` / `raise#` / `case error of {}`)
        // is non-trivial, so it thunkifies → forced only on demand → throws
        // with the same error-reporting behavior as evaluation.
        let deferred_simple: Vec<(VarId, usize)> =
            simple_bindings.iter().map(|(b, r)| (*b, *r)).collect();

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
            let code_ptr = compile_nested_body(
                &mut args,
                NestedFnSpec {
                    name: lambda_name,
                    arg_binder: Some(lam_binder),
                    captured_offset: CLOSURE_CAPTURED_OFFSET,
                    capture_vars: sorted_fvs,
                    body_tree: &lam_body_tree,
                    tail: TailCtx::Tail,
                },
            )?;
            args.builder.ins().store(
                MemFlags::trusted(),
                code_ptr,
                closure_ptr,
                CLOSURE_CODE_PTR_OFFSET,
            );

            // Capture slots were already zeroed at pre-alloc time (Phase 1,
            // via emit_alloc_zeroed) — that's what makes them GC-safe across
            // the gap between this pre-alloc and this fill.
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
        // A field's free vars, not just a direct `Var` node, can reach a
        // Phase-3c simple binder (e.g. `App g k` with `k` deferred) — matching
        // only direct Var children (M1) let such a field fill eagerly in this
        // phase, before `k` is bound, silently dropping it from the thunk's
        // captures (`compute_captures`'s `keep` filter has no error path).
        // A plain fn, not a capturing closure: `field_deferred_deps` is called
        // both before and after the mutable `args.sess` reborrows in the loop
        // below (`emit_subtree`/`emit_thunk`), so a closure holding
        // `&args.sess.free_vars_idx` across that whole span would conflict
        // with those reborrows. Taking the index by parameter instead means
        // each call borrows `args.sess.free_vars_idx` only for its own
        // expression.
        fn field_deferred_deps(
            free_vars_idx: &tidepool_repr::free_vars::FreeVarsIndex,
            simple_binder_set: &FxHashSet<VarId>,
            f_idx: usize,
        ) -> FxHashSet<VarId> {
            free_vars_idx
                .free_vars_at(f_idx)
                .into_iter()
                .filter(|v| simple_binder_set.contains(v))
                .collect()
        }
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
                    !field_deferred_deps(&args.sess.free_vars_idx, &simple_binder_set, f_idx)
                        .is_empty()
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

        // Bind deferred simple bindings in topological order (deps first) so each
        // thunk captures its already-bound siblings (see Phase 3c below).
        let deferred_simple =
            topo_sort_deferred_simple(deferred_simple, bindings, &args.sess.free_vars_idx);

        let mut deferred_con_deps: Vec<DeferredConDep> = Vec::with_capacity(deferred_cons.len());
        for (_, ptr, field_indices) in &deferred_cons {
            let deps: FxHashSet<VarId> = field_indices
                .iter()
                .flat_map(|&f_idx| {
                    field_deferred_deps(&args.sess.free_vars_idx, &simple_binder_set, f_idx)
                })
                .collect();
            deferred_con_deps.push(DeferredConDep {
                ptr: *ptr,
                field_indices: field_indices.clone(),
                remaining_deps: deps,
                filled: false,
            });
        }

        // Store deferred state for the Phase 3c post-step + LetRecFinish
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

        // Phase 3c: bind deferred simple bindings in TOPOLOGICAL order (deps
        // first). Lazy-default: thunkify the RHS, EXCEPT when it is trivially
        // resolvable now — a WHNF / strict-primop expr (`is_trivial_field`) all
        // of whose free vars are already in env — which we evaluate eagerly
        // instead (fast path, no thunk; a Var alias RHS takes this path too).
        // Topo order guarantees each binding's deferred-simple deps are
        // already in env, so `emit_thunk` captures them rather than dropping;
        // a true cycle (the `cycle` Known-Limit) leaves a dep unbound and
        // resolves to unresolved-on-force, unchanged. The post-step then fills
        // any closure captures / deferred Con fields that depended on this
        // binder with its (thunk or eager) value. Errors were poisoned in 2.5.
        let letrec_binders: FxHashSet<VarId> = bindings.iter().map(|(b, _)| *b).collect();
        for (binder, rhs_idx) in deferred_simple.iter() {
            let fvs = args.sess.free_vars_idx.free_vars_at(*rhs_idx);
            let resolvable_now = is_trivial_field(*rhs_idx, args.sess.tree)
                && fvs.iter().all(|v| args.ctx.env.contains_key(v));
            let sv = if resolvable_now {
                emit_subtree(
                    EmitArgs {
                        ctx: args.ctx,
                        sess: args.sess,
                        builder: args.builder,
                        tail: TailCtx::NonTail,
                    },
                    *rhs_idx,
                )?
            } else {
                // Knot-tying for recursive VALUE bindings (corecursive knots
                // like base `cycle`'s floated `xs' = xs ++ xs'`): captures
                // naming letrec binders not yet in env — self included — are
                // emitted as null placeholder slots and patched by
                // `letrec_post_simple_step` when the awaited binder lands
                // (the same pending-capture machinery the Lam pre-alloc knot
                // uses). Without this, such a capture would be silently
                // DROPPED by the capture filter, hitting unresolved_var_trap
                // on force instead.
                let promised: FxHashSet<VarId> = fvs
                    .iter()
                    .filter(|v| !args.ctx.env.contains_key(v) && letrec_binders.contains(v))
                    .copied()
                    .collect();
                let (sv, pending) = emit_thunk_promised(
                    EmitArgs {
                        ctx: args.ctx,
                        sess: args.sess,
                        builder: args.builder,
                        tail: TailCtx::NonTail,
                    },
                    *rhs_idx,
                    if promised.is_empty() {
                        None
                    } else {
                        Some(&promised)
                    },
                )?;
                if !pending.is_empty() {
                    let SsaVal::HeapPtr(thunk_ptr) = sv else {
                        unreachable!("emit_thunk_promised returns a heap ptr")
                    };
                    let state = args.ctx.letrec_state_mut(state_idx);
                    for (awaited, offset) in pending {
                        state
                            .pending_capture_updates
                            .entry(awaited)
                            .or_default()
                            .push(ClosureCaptureSlot {
                                closure_ptr: thunk_ptr,
                                offset,
                            });
                    }
                }
                sv
            };
            args.ctx.trace_scope(&format!(
                "{} LetRec(simple) {:?}",
                if resolvable_now { "insert" } else { "thunk" },
                binder
            ));
            args.ctx.env.insert(*binder, sv);
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
            if !dep.filled && dep.remaining_deps.is_empty() {
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
                dep.filled = true;
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
            if dep.filled {
                continue;
            }
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
    field_indices: Vec<usize>,
    /// Simple bindings this Con depends on. Entries removed as deps are satisfied.
    remaining_deps: FxHashSet<VarId>,
    /// Whether the fields have already been stored. An EXPLICIT done flag: an
    /// unfilled zero-field Con is otherwise indistinguishable from a completed
    /// one, so `field_indices` being empty can't serve as the sentinel. Phase
    /// 3c fills once and sets this; phase 3d skips deps already `filled`.
    filled: bool,
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
    let size = builder.ins().iconst(types::I32, LIT_TOTAL_SIZE as i64);
    builder.ins().store(MemFlags::trusted(), size, ptr, 1);

    match lit {
        Literal::LitInt(n) => {
            let lit_tag = builder.ins().iconst(types::I8, LIT_TAG_INT as i64);
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
            let lit_tag = builder.ins().iconst(types::I8, LIT_TAG_WORD as i64);
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
            let lit_tag = builder.ins().iconst(types::I8, LIT_TAG_CHAR as i64);
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
            let lit_tag = builder.ins().iconst(types::I8, LIT_TAG_FLOAT as i64);
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
            let lit_tag = builder.ins().iconst(types::I8, LIT_TAG_DOUBLE as i64);
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
    _counter: &mut u32,
) -> Result<SsaVal, EmitError> {
    // Anonymous data: same `add_function` multi-round collision concern as
    // `emit_lit_string` (a `__litba_<n>` name would clash in the live JITModule).
    let data_id = pipeline
        .module
        .declare_anonymous_data(false, false)
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
    let size = builder.ins().iconst(types::I32, LIT_TOTAL_SIZE as i64);
    builder.ins().store(MemFlags::trusted(), size, ptr, 1);
    let lit_tag = builder.ins().iconst(types::I8, LIT_TAG_BYTEARRAY as i64);
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
    _counter: &mut u32,
) -> Result<SsaVal, EmitError> {
    // Create data object: [len: u64][bytes...]. Anonymous: a session machine
    // adds fragments into the SAME live JITModule via `add_function`, so a
    // per-compile counter name (`__litstr_<n>`) would collide across rounds
    // ("Duplicate definition of identifier"). The blob needs no stable name.
    let data_id = pipeline
        .module
        .declare_anonymous_data(false, false)
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

    let local_data = pipeline.module.declare_data_in_func(data_id, builder.func);
    let data_ptr = builder.ins().symbol_value(types::I64, local_data);

    let ptr = emit_alloc_fast_path(builder, vmctx, LIT_TOTAL_SIZE, gc_sig, oom_func);

    let tag = builder.ins().iconst(types::I8, layout::TAG_LIT as i64);
    builder.ins().store(MemFlags::trusted(), tag, ptr, 0);
    let size = builder.ins().iconst(types::I32, LIT_TOTAL_SIZE as i64);
    builder.ins().store(MemFlags::trusted(), size, ptr, 1);
    let lit_tag = builder.ins().iconst(types::I8, LIT_TAG_STRING as i64);
    builder
        .ins()
        .store(MemFlags::trusted(), lit_tag, ptr, LIT_TAG_OFFSET);
    builder
        .ins()
        .store(MemFlags::trusted(), data_ptr, ptr, LIT_VALUE_OFFSET);

    builder.declare_value_needs_stack_map(ptr);
    Ok(SsaVal::HeapPtr(ptr))
}

/// Demand a case scrutinee, propagating bottom before executing an alternative.
/// Ordinary closures are WHNF; constructor fields remain lazy.
pub(crate) fn demand_whnf_ssaval(
    pipeline: &mut CodegenPipeline,
    builder: &mut FunctionBuilder,
    vmctx: Value,
    val: SsaVal,
) -> Result<SsaVal, EmitError> {
    let SsaVal::HeapPtr(ptr) = val else {
        return Ok(val);
    };
    let demand_fn = pipeline
        .module
        .declare_function(
            "heap_demand",
            Linkage::Import,
            &crate::emit::heap_force_sig(pipeline.isa.default_call_conv()),
        )
        .map_err(|e| EmitError::CraneliftError(e.to_string()))?;
    let demand_ref = pipeline
        .module
        .declare_func_in_func(demand_fn, builder.func);
    let call = builder.ins().call(demand_ref, &[vmctx, ptr]);
    let forced = builder.inst_results(call)[0];
    builder.declare_value_needs_stack_map(forced);
    let poison = builder
        .ins()
        .iconst(types::I64, crate::host_fns::error_poison_ptr() as i64);
    let failed = builder.ins().icmp(IntCC::Equal, forced, poison);
    let fail = builder.create_block();
    let ready = builder.create_block();
    builder.ins().brif(failed, fail, &[], ready, &[]);
    builder.switch_to_block(fail);
    builder.seal_block(fail);
    builder.ins().return_(&[forced]);
    builder.switch_to_block(ready);
    builder.seal_block(ready);
    Ok(SsaVal::HeapPtr(forced))
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
                .declare_function(
                    "heap_force",
                    Linkage::Import,
                    &crate::emit::heap_force_sig(pipeline.isa.default_call_conv()),
                )
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
            let size = builder.ins().iconst(types::I32, LIT_TOTAL_SIZE as i64);
            builder.ins().store(MemFlags::trusted(), size, ptr, 1);
            let lit_tag_val = builder.ins().iconst(types::I8, lit_tag as i64);
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

#[cfg(test)]
mod topo_sort_golden_tests {
    //! Golden tests pinning `topo_sort_deferred_simple`'s exact output order —
    //! including its tie-break among independent bindings and its behavior on
    //! a genuine cycle. Each test's expected order was derived by hand-tracing
    //! the implementation (see the reasoning left in each test's comment), not
    //! copied from a run, so a divergence here is a real behavior change, not
    //! a stale golden value.
    use super::*;

    /// Build a tiny tree of `Var`/`Lit` "rhs" fragments, one per binder, in
    /// the given order. `deps[i]` lists the binders binding `i`'s rhs
    /// references (as `Var` nodes summed via a `PrimOp` so `free_vars` sees
    /// all of them); an empty dep list gets a `Lit` rhs (no free vars).
    /// Returns `(tree, bindings)` where `bindings[i] = (VarId(i as u64+1),
    /// rhs_idx)` — usable as both `all_bindings` and (reordered/filtered) the
    /// `deferred_simple` argument.
    fn build_bindings(
        deps: &[(&'static str, &'static [&'static str])],
    ) -> (CoreExpr, FxHashMap<&'static str, VarId>) {
        let mut name_to_var: FxHashMap<&'static str, VarId> = FxHashMap::default();
        for (i, (name, _)) in deps.iter().enumerate() {
            name_to_var.insert(name, VarId((i + 1) as u64));
        }
        let mut nodes: Vec<CoreFrame<usize>> = Vec::new();
        let mut rhs_idx_for: FxHashMap<&'static str, usize> = FxHashMap::default();
        for (name, ds) in deps {
            let idx = if ds.is_empty() {
                nodes.push(CoreFrame::Lit(tidepool_repr::types::Literal::LitInt(0)));
                nodes.len() - 1
            } else {
                let var_idxs: Vec<usize> = ds
                    .iter()
                    .map(|d| {
                        nodes.push(CoreFrame::Var(name_to_var[d]));
                        nodes.len() - 1
                    })
                    .collect();
                nodes.push(CoreFrame::PrimOp {
                    op: tidepool_repr::types::PrimOpKind::IntAdd,
                    args: var_idxs,
                });
                nodes.len() - 1
            };
            rhs_idx_for.insert(name, idx);
        }
        // A tree needs a root; the last-pushed rhs already satisfies "root is
        // the last node" for the LAST binding only, so make everything
        // reachable via a final tuple-like Con root referencing every rhs —
        // topo_sort_deferred_simple only ever indexes into `tree.nodes`
        // directly by the rhs indices we recorded, so this root's shape
        // doesn't matter beyond keeping every rhs index in-bounds.
        let all_idxs: Vec<usize> = deps.iter().map(|(name, _)| rhs_idx_for[name]).collect();
        nodes.push(CoreFrame::Con {
            tag: tidepool_repr::types::DataConId(0),
            fields: all_idxs,
        });
        (RecursiveTree { nodes }, name_to_var)
    }

    fn bindings_for(
        deps: &[(&'static str, &'static [&'static str])],
        tree: &CoreExpr,
        vars: &FxHashMap<&'static str, VarId>,
    ) -> Vec<(VarId, usize)> {
        // Recover each name's rhs index the same way build_bindings assigned
        // it: re-walk in the same order, since build_bindings pushed rhs
        // nodes (and their Var/PrimOp dep nodes) in `deps` order before the
        // trailing Con root.
        let mut idx = 0usize;
        let mut out = Vec::with_capacity(deps.len());
        for (name, ds) in deps {
            let rhs_idx = if ds.is_empty() {
                let i = idx;
                idx += 1;
                i
            } else {
                idx += ds.len(); // skip the Var nodes
                let i = idx;
                idx += 1; // the PrimOp node itself
                i
            };
            debug_assert!(matches!(
                &tree.nodes[rhs_idx],
                CoreFrame::Lit(_) | CoreFrame::PrimOp { .. }
            ));
            out.push((vars[name], rhs_idx));
        }
        out
    }

    fn names(
        sorted: &[(VarId, usize)],
        vars: &FxHashMap<&'static str, VarId>,
    ) -> Vec<&'static str> {
        let rev: FxHashMap<VarId, &'static str> = vars.iter().map(|(k, v)| (*v, *k)).collect();
        sorted.iter().map(|(v, _)| rev[v]).collect()
    }

    #[test]
    fn independent_bindings_preserve_input_order() {
        // No dependencies among a/b/c: every binding is unblocked in the
        // first pass, so the output is exactly the input order.
        let deps: &[(&'static str, &'static [&'static str])] =
            &[("c", &[]), ("a", &[]), ("b", &[])];
        let (tree, vars) = build_bindings(deps);
        let all_bindings = bindings_for(deps, &tree, &vars);
        let deferred_simple = all_bindings.clone();
        let free_vars_idx = tidepool_repr::free_vars::FreeVarsIndex::compute(&tree);
        let sorted = topo_sort_deferred_simple(deferred_simple, &all_bindings, &free_vars_idx);
        assert_eq!(names(&sorted, &vars), vec!["c", "a", "b"]);
    }

    #[test]
    fn chain_resolves_regardless_of_input_order() {
        // a <- b <- c (c depends on b depends on a). Fed in REVERSE
        // dependency order [c, b, a]: round 1 resolves only `a` (the only
        // binding with no unmet deps); round 2 resolves `b`; round 3
        // resolves `c`. Final order is the true dependency order regardless
        // of input order.
        let deps: &[(&'static str, &'static [&'static str])] =
            &[("c", &["b"]), ("b", &["a"]), ("a", &[])];
        let (tree, vars) = build_bindings(deps);
        let all_bindings = bindings_for(deps, &tree, &vars);
        let deferred_simple = all_bindings.clone();
        let free_vars_idx = tidepool_repr::free_vars::FreeVarsIndex::compute(&tree);
        let sorted = topo_sort_deferred_simple(deferred_simple, &all_bindings, &free_vars_idx);
        assert_eq!(names(&sorted, &vars), vec!["a", "b", "c"]);
    }

    #[test]
    fn diamond_ties_break_by_input_order() {
        // a -> {c, b} -> d (both b and c depend only on a; d depends on
        // both). Fed as [a, c, b, d]: `a` has in-degree 0 and resolves
        // first, which frees both `c` and `b`; the min-heap then pops `c`
        // before `b` purely because `c` has the earlier `NodeIndex` (input
        // order), not because of any dependency between them. `d` only
        // becomes ready once both are resolved. Net result: exactly the
        // input order.
        let deps: &[(&'static str, &'static [&'static str])] =
            &[("a", &[]), ("c", &["a"]), ("b", &["a"]), ("d", &["b", "c"])];
        let (tree, vars) = build_bindings(deps);
        let all_bindings = bindings_for(deps, &tree, &vars);
        let deferred_simple = all_bindings.clone();
        let free_vars_idx = tidepool_repr::free_vars::FreeVarsIndex::compute(&tree);
        let sorted = topo_sort_deferred_simple(deferred_simple, &all_bindings, &free_vars_idx);
        assert_eq!(names(&sorted, &vars), vec!["a", "c", "b", "d"]);
    }

    #[test]
    fn genuine_cycle_falls_through_unordered_in_input_order() {
        // x depends on y, y depends on x: neither ever becomes unblocked, so
        // the fixed-point loop makes zero progress and both are appended
        // (unresolved) in their original relative order — the documented
        // `cycle` Known-Limit fallback.
        let deps: &[(&'static str, &'static [&'static str])] = &[("x", &["y"]), ("y", &["x"])];
        let (tree, vars) = build_bindings(deps);
        let all_bindings = bindings_for(deps, &tree, &vars);
        let deferred_simple = all_bindings.clone();
        let free_vars_idx = tidepool_repr::free_vars::FreeVarsIndex::compute(&tree);
        let sorted = topo_sort_deferred_simple(deferred_simple, &all_bindings, &free_vars_idx);
        assert_eq!(names(&sorted, &vars), vec!["x", "y"]);
    }

    #[test]
    fn self_reference_is_never_blocked_by_itself() {
        // z's rhs references z itself (a corecursive value knot). The DFS
        // that builds `reachable_deferred` explicitly excludes the start
        // node from its own reached set, so self-reference does not block —
        // z resolves in the very first pass alongside an unrelated
        // independent binding `w`, in input order.
        let deps: &[(&'static str, &'static [&'static str])] = &[("z", &["z"]), ("w", &[])];
        let (tree, vars) = build_bindings(deps);
        let all_bindings = bindings_for(deps, &tree, &vars);
        let deferred_simple = all_bindings.clone();
        let free_vars_idx = tidepool_repr::free_vars::FreeVarsIndex::compute(&tree);
        let sorted = topo_sort_deferred_simple(deferred_simple, &all_bindings, &free_vars_idx);
        assert_eq!(names(&sorted, &vars), vec!["z", "w"]);
    }
}
