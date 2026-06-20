use crate::emit::expr::{ensure_heap_ptr, force_thunk_ssaval};
use crate::emit::*;
use cranelift_codegen::ir::{
    self, condcodes::IntCC, types, AbiParam, BlockArg, InstBuilder, MemFlags, Signature, Value,
};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::{Linkage, Module};
use tidepool_repr::{Alt, AltCon, Literal, VarId};

/// Emit Case dispatch. The scrutinee has already been evaluated (stack-safe).
pub fn emit_case(
    args: EmitArgs,
    scrut: SsaVal,
    binder: &VarId,
    alts: &[Alt<usize>],
) -> Result<SsaVal, EmitError> {
    // 1. Scrutinee already evaluated
    let scrut_ptr = scrut.value();

    // 2. Bind case binder (save old value for restore)
    // NOTE: EnvGuard cannot be used here because it would borrow ctx.env mutably,
    // preventing the use of ctx in subsequent emit_* calls.
    let old_case_binder = args.ctx.env.insert(*binder, scrut);

    // 3. Classify alts
    let data_alts: Vec<_> = alts
        .iter()
        .filter(|alt| matches!(alt.con, AltCon::DataAlt(_)))
        .collect();
    let lit_alts: Vec<_> = alts
        .iter()
        .filter(|alt| matches!(alt.con, AltCon::LitAlt(_)))
        .collect();
    let default_alt = alts.iter().find(|alt| matches!(alt.con, AltCon::Default));

    // 4. Create merge block
    let merge_block = args.builder.create_block();
    args.builder.append_block_param(merge_block, types::I64);

    // 5. Dispatch
    if !data_alts.is_empty() {
        emit_data_dispatch(
            EmitArgs {
                ctx: args.ctx,
                sess: args.sess,
                builder: args.builder,
                tail: args.tail,
            },
            scrut_ptr,
            &data_alts,
            default_alt,
            merge_block,
        )?;
    } else if !lit_alts.is_empty() {
        emit_lit_dispatch(
            EmitArgs {
                ctx: args.ctx,
                sess: args.sess,
                builder: args.builder,
                tail: args.tail,
            },
            scrut,
            &lit_alts,
            default_alt,
            merge_block,
        )?;
    } else if let Some(alt) = default_alt {
        // Default only
        let result = EmitContext::emit_node(
            EmitArgs {
                ctx: args.ctx,
                sess: args.sess,
                builder: args.builder,
                tail: args.tail,
            },
            alt.body,
        )?;
        let result_ptr = ensure_heap_ptr(
            args.builder,
            args.sess.vmctx,
            args.sess.gc_sig,
            args.sess.oom_func,
            result,
        );
        args.builder
            .ins()
            .jump(merge_block, &[BlockArg::Value(result_ptr)]);
    } else {
        // No alts? Call runtime_case_trap to handle pending errors gracefully.
        emit_case_trap(
            args.sess,
            args.builder,
            &args.ctx.current_fn,
            scrut_ptr,
            &[],
            merge_block,
        )?;
    }

    // Seal merge block
    args.builder.seal_block(merge_block);

    // Switch to merge block
    args.builder.switch_to_block(merge_block);
    let result = args.builder.block_params(merge_block)[0];
    args.builder.declare_value_needs_stack_map(result);
    args.ctx.declare_env(args.builder);

    // 6. Restore case binder
    args.ctx.env.restore(*binder, old_case_binder);

    Ok(SsaVal::HeapPtr(result))
}

fn emit_data_dispatch(
    args: EmitArgs,
    initial_scrut_ptr: Value,
    data_alts: &[&Alt<usize>],
    default_alt: Option<&Alt<usize>>,
    merge_block: ir::Block,
) -> Result<(), EmitError> {
    // 1. Force if needed (tag < 2: Closure or Thunk)
    let tag = args
        .builder
        .ins()
        .load(types::I8, MemFlags::trusted(), initial_scrut_ptr, 0);
    let needs_force = args.builder.ins().icmp_imm(IntCC::UnsignedLessThan, tag, 2);

    let force_block = args.builder.create_block();
    let dispatch_block = args.builder.create_block();
    args.builder.append_block_param(dispatch_block, types::I64);

    args.builder.ins().brif(
        needs_force,
        force_block,
        &[],
        dispatch_block,
        &[BlockArg::Value(initial_scrut_ptr)],
    );

    // Force block: call host_fns::heap_force
    args.builder.switch_to_block(force_block);
    args.builder.seal_block(force_block);

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

    let call = args
        .builder
        .ins()
        .call(force_ref, &[args.sess.vmctx, initial_scrut_ptr]);
    let force_result = args.builder.inst_results(call)[0];
    args.builder.declare_value_needs_stack_map(force_result);
    args.builder
        .ins()
        .jump(dispatch_block, &[BlockArg::Value(force_result)]);

    // Dispatch block: actual pattern matching starts here
    args.builder.switch_to_block(dispatch_block);
    args.builder.seal_block(dispatch_block);
    let scrut_ptr = args.builder.block_params(dispatch_block)[0];
    args.builder.declare_value_needs_stack_map(scrut_ptr);

    // Load con_tag as u64 from offset 8
    let con_tag =
        args.builder
            .ins()
            .load(types::I64, MemFlags::trusted(), scrut_ptr, CON_TAG_OFFSET);

    // Runtime Lit-tolerance: a literal materialized on the Rust side (e.g. the
    // vendored aeson `Number`'s raw LitDouble field, see
    // tidepool-bridge/src/json.rs) reaches a data case on a boxed-literal
    // wrapper constructor (I#/W#/C#/F#/D#) as a *bare* Lit heap object, not a
    // boxed Con. Its (garbage) con_tag matches no alt, so the chain below would
    // fall through to the trap. Detect the (at most one) wrapper alt at emit
    // time \u2014 zero cost for ordinary ADT cases \u2014 and, when the scrutinee is a
    // Lit at runtime, route to that alt. The wrapper alt's single binder ends up
    // bound to a pointer-to-Lit in BOTH paths (the con path loads field0, which
    // is itself a pointer to a Lit; the Lit path uses the whole scrutinee), so
    // the body's downstream unboxing sees an identical representation. The alt
    // block is given a binder parameter so both paths share one emitted body.
    let wrapper_pos = data_alts.iter().position(
        |alt| matches!(&alt.con, AltCon::DataAlt(tag) if args.sess.lit_wrappers.is_wrapper(*tag)),
    );

    let wrapper_block = wrapper_pos.map(|_| {
        let b = args.builder.create_block();
        args.builder.append_block_param(b, types::I64);
        b
    });

    if let Some(wb) = wrapper_block {
        let kind_tag = args
            .builder
            .ins()
            .load(types::I8, MemFlags::trusted(), scrut_ptr, 0);
        let is_lit = args
            .builder
            .ins()
            .icmp_imm(IntCC::Equal, kind_tag, TAG_LIT as i64);
        let con_path_block = args.builder.create_block();
        args.builder.ins().brif(
            is_lit,
            wb,
            &[BlockArg::Value(scrut_ptr)],
            con_path_block,
            &[],
        );
        args.builder.switch_to_block(con_path_block);
        args.builder.seal_block(con_path_block);
    }

    // Use comparison chain instead of jump table because DataConIds are large
    // GHC Uniques (arbitrary u64 values), not small sequential integers.
    for (alt_idx, &alt) in data_alts.iter().enumerate() {
        let AltCon::DataAlt(tag) = &alt.con else {
            continue;
        };
        let is_wrapper = Some(alt_idx) == wrapper_pos;

        let alt_block = if is_wrapper {
            wrapper_block.expect("wrapper_block is Some whenever wrapper_pos is Some")
        } else {
            args.builder.create_block()
        };
        let next_check_block = args.builder.create_block();

        let tag_val = args.builder.ins().iconst(types::I64, tag.0 as i64);
        let eq = args.builder.ins().icmp(IntCC::Equal, con_tag, tag_val);
        if is_wrapper {
            // Con path: the wrapper has exactly one field \u2014 a pointer to a Lit.
            // Pass it as the shared binder parameter (matching the Lit path,
            // which passes the scrutinee Lit itself).
            let field0 = args.builder.ins().load(
                types::I64,
                MemFlags::trusted(),
                scrut_ptr,
                CON_FIELDS_OFFSET,
            );
            args.builder.declare_value_needs_stack_map(field0);
            args.builder.ins().brif(
                eq,
                alt_block,
                &[BlockArg::Value(field0)],
                next_check_block,
                &[],
            );
        } else {
            args.builder
                .ins()
                .brif(eq, alt_block, &[], next_check_block, &[]);
        }

        // Emit alt body
        args.builder.switch_to_block(alt_block);
        // For a wrapper alt block both predecessors (the Lit branch above and
        // the con-tag branch just emitted) are now wired, so sealing is safe.
        args.builder.seal_block(alt_block);
        args.ctx.declare_env(args.builder);

        // Bind pattern variables \u2014 do NOT force thunked fields.
        // In Haskell, case alt binders are lazy. Thunked Con fields
        // remain as thunks until used in a strict context (case scrutiny,
        // primop args, etc.). Forcing here causes infinite loops for
        // self-referencing structures like `xs = 1 : map (+1) xs`.
        //
        // INVARIANT: All strict consumers must force thunked values before
        // reading heap layout. The forcing points are:
        //   - emit_lit_dispatch: force_thunk_ssaval on scrutinee
        //   - emit_data_dispatch: tag < 2 check \u2192 heap_force on scrutinee
        //   - PrimOp collapse: force_thunk_ssaval on all args
        //   - App collapse: tag check \u2192 heap_force on fun position
        //   - unbox_int/unbox_double/unbox_float: defensive trap on TAG_THUNK
        // See force_thunk_ssaval in expr.rs.
        let mut scope = EnvScope::new();
        // NOTE: EnvGuard cannot be used here because it would borrow ctx.env
        // mutably, preventing the use of ctx in emit_node.
        if is_wrapper {
            // Single binder bound to the binder parameter (a pointer to a Lit).
            let binder_val = args.builder.block_params(alt_block)[0];
            args.builder.declare_value_needs_stack_map(binder_val);
            if let Some(&binder) = alt.binders.first() {
                args.ctx
                    .env
                    .insert_scoped(&mut scope, binder, SsaVal::HeapPtr(binder_val));
            }
        } else {
            for (i, &binder) in alt.binders.iter().enumerate() {
                let offset = CON_FIELDS_OFFSET + (8 * i as i32);
                let field_val =
                    args.builder
                        .ins()
                        .load(types::I64, MemFlags::trusted(), scrut_ptr, offset);
                args.builder.declare_value_needs_stack_map(field_val);
                args.ctx
                    .env
                    .insert_scoped(&mut scope, binder, SsaVal::HeapPtr(field_val));
            }
        }

        let result = EmitContext::emit_node(
            EmitArgs {
                ctx: args.ctx,
                sess: args.sess,
                builder: args.builder,
                tail: args.tail,
            },
            alt.body,
        )?;
        let result_ptr = ensure_heap_ptr(
            args.builder,
            args.sess.vmctx,
            args.sess.gc_sig,
            args.sess.oom_func,
            result,
        );
        args.builder
            .ins()
            .jump(merge_block, &[BlockArg::Value(result_ptr)]);

        // Restore pattern variable bindings
        args.ctx.env.restore_scope(scope);

        // Continue to next check
        args.builder.switch_to_block(next_check_block);
        args.builder.seal_block(next_check_block);
    }

    // Default or trap
    if let Some(alt) = default_alt {
        args.ctx.declare_env(args.builder);
        let result = EmitContext::emit_node(
            EmitArgs {
                ctx: args.ctx,
                sess: args.sess,
                builder: args.builder,
                tail: args.tail,
            },
            alt.body,
        )?;
        let result_ptr = ensure_heap_ptr(
            args.builder,
            args.sess.vmctx,
            args.sess.gc_sig,
            args.sess.oom_func,
            result,
        );
        args.builder
            .ins()
            .jump(merge_block, &[BlockArg::Value(result_ptr)]);
    } else {
        emit_case_trap(
            args.sess,
            args.builder,
            &args.ctx.current_fn,
            scrut_ptr,
            data_alts,
            merge_block,
        )?;
    }

    Ok(())
}

/// Emit a call to `runtime_case_trap` instead of a bare `trap user2`.
/// Passes the scrutinee pointer and expected alt tags for diagnostic output.
fn emit_case_trap(
    sess: &mut EmitSession,
    builder: &mut FunctionBuilder,
    fn_name: &str,
    scrut_ptr: Value,
    data_alts: &[&Alt<usize>],
    merge_block: ir::Block,
) -> Result<(), EmitError> {
    // Leak the enclosing-function name for the diagnostic (bounded by the
    // number of case sites per compile; diagnostics-only).
    let name_static: &'static str = Box::leak(fn_name.to_string().into_boxed_str());
    let name_ptr = builder
        .ins()
        .iconst(types::I64, name_static.as_ptr() as i64);
    let name_len = builder.ins().iconst(types::I64, name_static.len() as i64);
    // Collect expected tags
    let tags: Vec<u64> = data_alts
        .iter()
        .filter_map(|alt| {
            if let AltCon::DataAlt(tag) = &alt.con {
                Some(tag.0)
            } else {
                None
            }
        })
        .collect();

    // Store tags on stack
    let num_alts = tags.len();
    let ss = builder.create_sized_stack_slot(ir::StackSlotData::new(
        ir::StackSlotKind::ExplicitSlot,
        (num_alts * 8) as u32,
        3, // align 8
    ));
    for (i, &tag) in tags.iter().enumerate() {
        let tag_val = builder.ins().iconst(types::I64, tag as i64);
        builder.ins().stack_store(tag_val, ss, (i * 8) as i32);
    }
    let tags_addr = builder.ins().stack_addr(types::I64, ss, 0);

    let trap_fn = sess
        .pipeline
        .module
        .declare_function("runtime_case_trap", Linkage::Import, &{
            let mut sig = Signature::new(sess.pipeline.isa.default_call_conv());
            sig.params.push(AbiParam::new(types::I64)); // scrut_ptr
            sig.params.push(AbiParam::new(types::I64)); // num_alts
            sig.params.push(AbiParam::new(types::I64)); // alt_tags
            sig.params.push(AbiParam::new(types::I64)); // fn name ptr
            sig.params.push(AbiParam::new(types::I64)); // fn name len
            sig.returns.push(AbiParam::new(types::I64)); // returns poison ptr
            sig
        })
        .map_err(|e| EmitError::CraneliftError(e.to_string()))?;
    let trap_ref = sess
        .pipeline
        .module
        .declare_func_in_func(trap_fn, builder.func);
    let num_alts_val = builder.ins().iconst(types::I64, num_alts as i64);
    let call = builder.ins().call(
        trap_ref,
        &[scrut_ptr, num_alts_val, tags_addr, name_ptr, name_len],
    );
    let result = builder.inst_results(call)[0];
    builder.ins().jump(merge_block, &[BlockArg::Value(result)]);
    Ok(())
}

fn emit_lit_dispatch(
    args: EmitArgs,
    scrut: SsaVal,
    lit_alts: &[&Alt<usize>],
    default_alt: Option<&Alt<usize>>,
    merge_block: ir::Block,
) -> Result<(), EmitError> {
    // Force thunked scrutinees: literal case dispatch is strict \u2014
    // ThunkCon fields extracted by data alt matching may still be thunks.
    let scrut = force_thunk_ssaval(args.sess.pipeline, args.builder, args.sess.vmctx, scrut)?;

    // Unbox scrutinee: Raw values are already unboxed, HeapPtr needs LIT_VALUE_OFFSET load
    let scrut_value = match scrut {
        SsaVal::Raw(v, _) => v,
        SsaVal::HeapPtr(ptr) => {
            args.builder
                .ins()
                .load(types::I64, MemFlags::trusted(), ptr, LIT_VALUE_OFFSET)
        }
    };

    for &alt in lit_alts {
        let alt_block = args.builder.create_block();
        let next_check_block = args.builder.create_block();

        if let AltCon::LitAlt(lit) = &alt.con {
            match lit {
                Literal::LitInt(n) => {
                    let lit_val = args.builder.ins().iconst(types::I64, *n);
                    let eq = args.builder.ins().icmp(IntCC::Equal, scrut_value, lit_val);
                    args.builder
                        .ins()
                        .brif(eq, alt_block, &[], next_check_block, &[]);
                }
                Literal::LitWord(n) => {
                    let lit_val = args.builder.ins().iconst(types::I64, *n as i64);
                    let eq = args.builder.ins().icmp(IntCC::Equal, scrut_value, lit_val);
                    args.builder
                        .ins()
                        .brif(eq, alt_block, &[], next_check_block, &[]);
                }
                Literal::LitChar(c) => {
                    let lit_val = args.builder.ins().iconst(types::I64, *c as i64);
                    let eq = args.builder.ins().icmp(IntCC::Equal, scrut_value, lit_val);
                    args.builder
                        .ins()
                        .brif(eq, alt_block, &[], next_check_block, &[]);
                }
                Literal::LitFloat(bits) => {
                    let scrut_f64 = args.builder.ins().bitcast(
                        types::F64,
                        MemFlags::new().with_endianness(ir::Endianness::Little),
                        scrut_value,
                    );
                    let lit_val = args.builder.ins().f64const(f64::from_bits(*bits));
                    let eq =
                        args.builder
                            .ins()
                            .fcmp(ir::condcodes::FloatCC::Equal, scrut_f64, lit_val);
                    args.builder
                        .ins()
                        .brif(eq, alt_block, &[], next_check_block, &[]);
                }
                Literal::LitDouble(bits) => {
                    let scrut_f64 = args.builder.ins().bitcast(
                        types::F64,
                        MemFlags::new().with_endianness(ir::Endianness::Little),
                        scrut_value,
                    );
                    let lit_val = args.builder.ins().f64const(f64::from_bits(*bits));
                    let eq =
                        args.builder
                            .ins()
                            .fcmp(ir::condcodes::FloatCC::Equal, scrut_f64, lit_val);
                    args.builder
                        .ins()
                        .brif(eq, alt_block, &[], next_check_block, &[]);
                }
                Literal::LitString(_) | Literal::LitByteArray(_) => {
                    return Err(EmitError::NotYetImplemented("LitString in Case".into()))
                }
            }
        }

        // Emit alt body
        args.builder.switch_to_block(alt_block);
        args.builder.seal_block(alt_block);
        args.ctx.declare_env(args.builder);
        let result = EmitContext::emit_node(
            EmitArgs {
                ctx: args.ctx,
                sess: args.sess,
                builder: args.builder,
                tail: args.tail,
            },
            alt.body,
        )?;
        let result_ptr = ensure_heap_ptr(
            args.builder,
            args.sess.vmctx,
            args.sess.gc_sig,
            args.sess.oom_func,
            result,
        );
        args.builder
            .ins()
            .jump(merge_block, &[BlockArg::Value(result_ptr)]);

        // Continue to next check
        args.builder.switch_to_block(next_check_block);
        args.builder.seal_block(next_check_block);
    }

    // Default or trap
    if let Some(alt) = default_alt {
        args.ctx.declare_env(args.builder);
        let result = EmitContext::emit_node(
            EmitArgs {
                ctx: args.ctx,
                sess: args.sess,
                builder: args.builder,
                tail: args.tail,
            },
            alt.body,
        )?;
        let result_ptr = ensure_heap_ptr(
            args.builder,
            args.sess.vmctx,
            args.sess.gc_sig,
            args.sess.oom_func,
            result,
        );
        args.builder
            .ins()
            .jump(merge_block, &[BlockArg::Value(result_ptr)]);
    } else {
        // No alts matched.
        // We pass empty data_alts since these are lit alts.
        emit_case_trap(
            args.sess,
            args.builder,
            &args.ctx.current_fn,
            scrut_value,
            &[],
            merge_block,
        )?;
    }

    Ok(())
}
