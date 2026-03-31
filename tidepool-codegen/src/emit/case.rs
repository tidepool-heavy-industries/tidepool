use crate::emit::expr::{emit_node, ensure_heap_ptr, force_thunk_ssaval};
use crate::emit::*;
use cranelift_codegen::ir::{
    self, condcodes::IntCC, types, AbiParam, BlockArg, InstBuilder, MemFlags, Signature, Value,
};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::{Linkage, Module};
use tidepool_repr::{Alt, AltCon, Literal, VarId};

/// Emit Case dispatch. The scrutinee has already been evaluated (stack-safe).
pub fn emit_case(
    state: &mut EmitState,
    scrut: SsaVal,
    binder: &VarId,
    alts: &[Alt<usize>],
) -> Result<SsaVal, EmitError> {
    // 1. Scrutinee already evaluated
    let scrut_ptr = scrut.value();

    // 2. Bind case binder (save old value for restore)
    // NOTE: EnvGuard cannot be used here because it would borrow ctx.env mutably,
    // preventing the use of ctx in subsequent emit_* calls.
    let old_case_binder = state.ctx.env.insert(*binder, scrut);

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
    let merge_block = state.builder.create_block();
    state.builder.append_block_param(merge_block, types::I64);

    // 5. Dispatch
    if !data_alts.is_empty() {
        emit_data_dispatch(
            state,
            scrut_ptr,
            &data_alts,
            default_alt,
            merge_block,
        )?;
    } else if !lit_alts.is_empty() {
        emit_lit_dispatch(
            state,
            scrut,
            &lit_alts,
            default_alt,
            merge_block,
        )?;
    } else if let Some(alt) = default_alt {
        // Default only
        let result = emit_node(state, alt.body)?;
        let result_ptr = ensure_heap_ptr(state.builder, state.sess.vmctx, state.sess.gc_sig, state.sess.oom_func, result);
        state.builder
            .ins()
            .jump(merge_block, &[BlockArg::Value(result_ptr)]);
    } else {
        // No alts? Call runtime_case_trap to handle pending errors gracefully.
        emit_case_trap(state.sess, state.builder, scrut_ptr, &[], merge_block)?;
    }

    // Seal merge block
    state.builder.seal_block(merge_block);

    // Switch to merge block
    state.builder.switch_to_block(merge_block);
    let result = state.builder.block_params(merge_block)[0];
    state.builder.declare_value_needs_stack_map(result);
    state.ctx.declare_env(state.builder);

    // 6. Restore case binder
    state.ctx.env.restore(*binder, old_case_binder);

    Ok(SsaVal::HeapPtr(result))
}

fn emit_data_dispatch(
    state: &mut EmitState,
    initial_scrut_ptr: Value,
    data_alts: &[&Alt<usize>],
    default_alt: Option<&Alt<usize>>,
    merge_block: ir::Block,
) -> Result<(), EmitError> {
    // 1. Force if needed (tag < 2: Closure or Thunk)
    let tag = state.builder
        .ins()
        .load(types::I8, MemFlags::trusted(), initial_scrut_ptr, 0);
    let needs_force = state.builder.ins().icmp_imm(IntCC::UnsignedLessThan, tag, 2);

    let force_block = state.builder.create_block();
    let dispatch_block = state.builder.create_block();
    state.builder.append_block_param(dispatch_block, types::I64);

    state.builder.ins().brif(
        needs_force,
        force_block,
        &[],
        dispatch_block,
        &[BlockArg::Value(initial_scrut_ptr)],
    );

    // Force block: call host_fns::heap_force
    state.builder.switch_to_block(force_block);
    state.builder.seal_block(force_block);

    let force_fn = state.sess
        .pipeline
        .module
        .declare_function("heap_force", Linkage::Import, &{
            let mut sig = Signature::new(state.sess.pipeline.isa.default_call_conv());
            sig.params.push(AbiParam::new(types::I64)); // vmctx
            sig.params.push(AbiParam::new(types::I64)); // thunk
            sig.returns.push(AbiParam::new(types::I64)); // result
            sig
        })
        .map_err(|e| EmitError::CraneliftError(e.to_string()))?;
    let force_ref = state.sess
        .pipeline
        .module
        .declare_func_in_func(force_fn, state.builder.func);

    let call = state.builder
        .ins()
        .call(force_ref, &[state.sess.vmctx, initial_scrut_ptr]);
    let force_result = state.builder.inst_results(call)[0];
    state.builder.declare_value_needs_stack_map(force_result);
    state.builder
        .ins()
        .jump(dispatch_block, &[BlockArg::Value(force_result)]);

    // Dispatch block: actual pattern matching starts here
    state.builder.switch_to_block(dispatch_block);
    state.builder.seal_block(dispatch_block);
    let scrut_ptr = state.builder.block_params(dispatch_block)[0];
    state.builder.declare_value_needs_stack_map(scrut_ptr);

    // Load con_tag as u64 from offset 8
    let con_tag = state.builder
        .ins()
        .load(types::I64, MemFlags::trusted(), scrut_ptr, CON_TAG_OFFSET);

    // Use comparison chain instead of jump table because DataConIds are large
    // GHC Uniques (arbitrary u64 values), not small sequential integers.
    for &alt in data_alts {
        let AltCon::DataAlt(tag) = &alt.con else {
            continue;
        };

        let alt_block = state.builder.create_block();
        let next_check_block = state.builder.create_block();

        let tag_val = state.builder.ins().iconst(types::I64, tag.0 as i64);
        let eq = state.builder.ins().icmp(IntCC::Equal, con_tag, tag_val);
        state.builder
            .ins()
            .brif(eq, alt_block, &[], next_check_block, &[]);

        // Emit alt body
        state.builder.switch_to_block(alt_block);
        state.builder.seal_block(alt_block);
        state.ctx.declare_env(state.builder);

        // Bind pattern variables — do NOT force thunked fields.
        // In Haskell, case alt binders are lazy. Thunked Con fields
        // remain as thunks until used in a strict context (case scrutiny,
        // primop args, etc.). Forcing here causes infinite loops for
        // self-referencing structures like `xs = 1 : map (+1) xs`.
        //
        // INVARIANT: All strict consumers must force thunked values before
        // reading heap layout. The forcing points are:
        //   - emit_lit_dispatch: force_thunk_ssaval on scrutinee
        //   - emit_data_dispatch: tag < 2 check → heap_force on scrutinee
        //   - PrimOp collapse: force_thunk_ssaval on all args
        //   - App collapse: tag check → heap_force on fun position
        //   - unbox_int/unbox_double/unbox_float: defensive trap on TAG_THUNK
        // See force_thunk_ssaval in expr.rs.
        let mut scope = EnvScope::new();
        // NOTE: EnvGuard cannot be used here because it would borrow ctx.env
        // mutably, preventing the use of ctx in emit_node.
        for (i, &binder) in alt.binders.iter().enumerate() {
            let offset = CON_FIELDS_OFFSET + (8 * i as i32);
            let field_val = state.builder
                .ins()
                .load(types::I64, MemFlags::trusted(), scrut_ptr, offset);
            state.builder.declare_value_needs_stack_map(field_val);
            state.ctx.env
                .insert_scoped(&mut scope, binder, SsaVal::HeapPtr(field_val));
        }

        let result = emit_node(state, alt.body)?;
        let result_ptr = ensure_heap_ptr(state.builder, state.sess.vmctx, state.sess.gc_sig, state.sess.oom_func, result);
        state.builder
            .ins()
            .jump(merge_block, &[BlockArg::Value(result_ptr)]);

        // Restore pattern variable bindings
        state.ctx.env.restore_scope(scope);

        // Continue to next check
        state.builder.switch_to_block(next_check_block);
        state.builder.seal_block(next_check_block);
    }

    // Default or trap
    if let Some(alt) = default_alt {
        state.ctx.declare_env(state.builder);
        let result = emit_node(state, alt.body)?;
        let result_ptr = ensure_heap_ptr(state.builder, state.sess.vmctx, state.sess.gc_sig, state.sess.oom_func, result);
        state.builder
            .ins()
            .jump(merge_block, &[BlockArg::Value(result_ptr)]);
    } else {
        emit_case_trap(state.sess, state.builder, scrut_ptr, data_alts, merge_block)?;
    }

    Ok(())
}

/// Emit a call to `runtime_case_trap` instead of a bare `trap user2`.
/// Passes the scrutinee pointer and expected alt tags for diagnostic output.
fn emit_case_trap(
    sess: &mut EmitSession,
    builder: &mut FunctionBuilder,
    scrut_ptr: Value,
    data_alts: &[&Alt<usize>],
    merge_block: ir::Block,
) -> Result<(), EmitError> {
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
            sig.returns.push(AbiParam::new(types::I64)); // returns poison ptr
            sig
        })
        .map_err(|e| EmitError::CraneliftError(e.to_string()))?;
    let trap_ref = sess
        .pipeline
        .module
        .declare_func_in_func(trap_fn, builder.func);
    let num_alts_val = builder.ins().iconst(types::I64, num_alts as i64);
    let call = builder
        .ins()
        .call(trap_ref, &[scrut_ptr, num_alts_val, tags_addr]);
    let result = builder.inst_results(call)[0];
    builder.ins().jump(merge_block, &[BlockArg::Value(result)]);
    Ok(())
}

fn emit_lit_dispatch(
    state: &mut EmitState,
    scrut: SsaVal,
    lit_alts: &[&Alt<usize>],
    default_alt: Option<&Alt<usize>>,
    merge_block: ir::Block,
) -> Result<(), EmitError> {
    // Force thunked scrutinees: literal case dispatch is strict —
    // ThunkCon fields extracted by data alt matching may still be thunks.
    let scrut = force_thunk_ssaval(state.sess.pipeline, state.builder, state.sess.vmctx, scrut)?;

    // Unbox scrutinee: Raw values are already unboxed, HeapPtr needs LIT_VALUE_OFFSET load
    let scrut_value = match scrut {
        SsaVal::Raw(v, _) => v,
        SsaVal::HeapPtr(ptr) => {
            state.builder
                .ins()
                .load(types::I64, MemFlags::trusted(), ptr, LIT_VALUE_OFFSET)
        }
    };

    for &alt in lit_alts {
        let alt_block = state.builder.create_block();
        let next_check_block = state.builder.create_block();

        if let AltCon::LitAlt(lit) = &alt.con {
            match lit {
                Literal::LitInt(n) => {
                    let lit_val = state.builder.ins().iconst(types::I64, *n);
                    let eq = state.builder.ins().icmp(IntCC::Equal, scrut_value, lit_val);
                    state.builder
                        .ins()
                        .brif(eq, alt_block, &[], next_check_block, &[]);
                }
                Literal::LitWord(n) => {
                    let lit_val = state.builder.ins().iconst(types::I64, *n as i64);
                    let eq = state.builder.ins().icmp(IntCC::Equal, scrut_value, lit_val);
                    state.builder
                        .ins()
                        .brif(eq, alt_block, &[], next_check_block, &[]);
                }
                Literal::LitChar(c) => {
                    let lit_val = state.builder.ins().iconst(types::I64, *c as i64);
                    let eq = state.builder.ins().icmp(IntCC::Equal, scrut_value, lit_val);
                    state.builder
                        .ins()
                        .brif(eq, alt_block, &[], next_check_block, &[]);
                }
                Literal::LitFloat(bits) => {
                    let scrut_f64 = state.builder.ins().bitcast(
                        types::F64,
                        MemFlags::new().with_endianness(ir::Endianness::Little),
                        scrut_value,
                    );
                    let lit_val = state.builder.ins().f64const(f64::from_bits(*bits));
                    let eq = state.builder
                        .ins()
                        .fcmp(ir::condcodes::FloatCC::Equal, scrut_f64, lit_val);
                    state.builder
                        .ins()
                        .brif(eq, alt_block, &[], next_check_block, &[]);
                }
                Literal::LitDouble(bits) => {
                    let scrut_f64 = state.builder.ins().bitcast(
                        types::F64,
                        MemFlags::new().with_endianness(ir::Endianness::Little),
                        scrut_value,
                    );
                    let lit_val = state.builder.ins().f64const(f64::from_bits(*bits));
                    let eq = state.builder
                        .ins()
                        .fcmp(ir::condcodes::FloatCC::Equal, scrut_f64, lit_val);
                    state.builder
                        .ins()
                        .brif(eq, alt_block, &[], next_check_block, &[]);
                }
                Literal::LitString(_) => {
                    return Err(EmitError::NotYetImplemented("LitString in Case".into()))
                }
            }
        }

        // Emit alt body
        state.builder.switch_to_block(alt_block);
        state.builder.seal_block(alt_block);
        state.ctx.declare_env(state.builder);
        let result = emit_node(state, alt.body)?;
        let result_ptr = ensure_heap_ptr(state.builder, state.sess.vmctx, state.sess.gc_sig, state.sess.oom_func, result);
        state.builder
            .ins()
            .jump(merge_block, &[BlockArg::Value(result_ptr)]);

        // Continue to next check
        state.builder.switch_to_block(next_check_block);
        state.builder.seal_block(next_check_block);
    }

    // Default or trap
    if let Some(alt) = default_alt {
        state.ctx.declare_env(state.builder);
        let result = emit_node(state, alt.body)?;
        let result_ptr = ensure_heap_ptr(state.builder, state.sess.vmctx, state.sess.gc_sig, state.sess.oom_func, result);
        state.builder
            .ins()
            .jump(merge_block, &[BlockArg::Value(result_ptr)]);
    } else {
        // No alts matched.
        // We pass empty data_alts since these are lit alts.
        emit_case_trap(state.sess, state.builder, scrut_value, &[], merge_block)?;
    }

    Ok(())
}
