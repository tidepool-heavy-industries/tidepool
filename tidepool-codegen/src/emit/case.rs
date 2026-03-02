use crate::emit::expr::ensure_heap_ptr;
use crate::emit::*;
use crate::pipeline::CodegenPipeline;
use cranelift_codegen::ir::{
    self, condcodes::IntCC, types, AbiParam, InstBuilder, MemFlags, Signature, TrapCode, Value,
};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::{Linkage, Module};
use tidepool_repr::{Alt, AltCon, CoreExpr, Literal, VarId};

/// Emit Case dispatch. The scrutinee has already been evaluated (stack-safe).
#[allow(clippy::too_many_arguments)]
pub fn emit_case(
    ctx: &mut EmitContext,
    pipeline: &mut CodegenPipeline,
    builder: &mut FunctionBuilder,
    vmctx: Value,
    gc_sig: ir::SigRef,
    oom_func: ir::FuncRef,
    tree: &CoreExpr,
    scrut: SsaVal,
    binder: &VarId,
    alts: &[Alt<usize>],
) -> Result<SsaVal, EmitError> {
    // 1. Scrutinee already evaluated
    let scrut_ptr = scrut.value();

    // 2. Bind case binder
    ctx.env.insert(*binder, scrut);

    // 3. Classify alts
    let mut data_alts = Vec::new();
    let mut lit_alts = Vec::new();
    let mut default_alt = None;

    for alt in alts {
        match &alt.con {
            AltCon::DataAlt(_) => data_alts.push(alt),
            AltCon::LitAlt(_) => lit_alts.push(alt),
            AltCon::Default => default_alt = Some(alt),
        }
    }

    // 4. Create merge block
    let merge_block = builder.create_block();
    builder.append_block_param(merge_block, types::I64);

    // 5. Dispatch
    if !data_alts.is_empty() {
        emit_data_dispatch(
            ctx,
            pipeline,
            builder,
            vmctx,
            gc_sig,
            oom_func,
            tree,
            scrut_ptr,
            &data_alts,
            default_alt,
            merge_block,
        )?;
    } else if !lit_alts.is_empty() {
        emit_lit_dispatch(
            ctx,
            pipeline,
            builder,
            vmctx,
            gc_sig,
            oom_func,
            tree,
            scrut,
            &lit_alts,
            default_alt,
            merge_block,
        )?;
    } else if let Some(alt) = default_alt {
        // Default only
        let result = ctx.emit_node(pipeline, builder, vmctx, gc_sig, oom_func, tree, alt.body)?;
        let result_ptr = ensure_heap_ptr(builder, vmctx, gc_sig, oom_func, result);
        builder.ins().jump(merge_block, &[result_ptr]);
    } else {
        // No alts? Trap.
        builder.ins().trap(TrapCode::unwrap_user(2));
    }

    // Seal merge block
    builder.seal_block(merge_block);

    // Switch to merge block
    builder.switch_to_block(merge_block);
    let result = builder.block_params(merge_block)[0];
    builder.declare_value_needs_stack_map(result);
    ctx.declare_env(builder);

    // 6. Clean up case binder
    ctx.env.remove(binder);

    Ok(SsaVal::HeapPtr(result))
}

#[allow(clippy::too_many_arguments)]
fn emit_data_dispatch(
    ctx: &mut EmitContext,
    pipeline: &mut CodegenPipeline,
    builder: &mut FunctionBuilder,
    vmctx: Value,
    gc_sig: ir::SigRef,
    oom_func: ir::FuncRef,
    tree: &CoreExpr,
    initial_scrut_ptr: Value,
    data_alts: &[&Alt<usize>],
    default_alt: Option<&Alt<usize>>,
    merge_block: ir::Block,
) -> Result<(), EmitError> {
    // 1. Force if needed (tag < 2: Closure or Thunk)
    let tag = builder
        .ins()
        .load(types::I8, MemFlags::trusted(), initial_scrut_ptr, 0);
    let needs_force = builder.ins().icmp_imm(IntCC::UnsignedLessThan, tag, 2);

    let force_block = builder.create_block();
    let dispatch_block = builder.create_block();
    builder.append_block_param(dispatch_block, types::I64);

    builder.ins().brif(
        needs_force,
        force_block,
        &[],
        dispatch_block,
        &[initial_scrut_ptr],
    );

    // Force block: call host_fns::heap_force
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

    let call = builder.ins().call(force_ref, &[vmctx, initial_scrut_ptr]);
    let force_result = builder.inst_results(call)[0];
    builder.declare_value_needs_stack_map(force_result);
    builder.ins().jump(dispatch_block, &[force_result]);

    // Dispatch block: actual pattern matching starts here
    builder.switch_to_block(dispatch_block);
    builder.seal_block(dispatch_block);
    let scrut_ptr = builder.block_params(dispatch_block)[0];
    builder.declare_value_needs_stack_map(scrut_ptr);

    // Load con_tag as u64 from offset 8
    let con_tag = builder
        .ins()
        .load(types::I64, MemFlags::trusted(), scrut_ptr, CON_TAG_OFFSET);

    // Use comparison chain instead of jump table because DataConIds are large
    // GHC Uniques (arbitrary u64 values), not small sequential integers.
    for &alt in data_alts {
        if let AltCon::DataAlt(tag) = &alt.con {
            let alt_block = builder.create_block();
            let next_check_block = builder.create_block();

            let tag_val = builder.ins().iconst(types::I64, tag.0 as i64);
            let eq = builder.ins().icmp(IntCC::Equal, con_tag, tag_val);
            builder
                .ins()
                .brif(eq, alt_block, &[], next_check_block, &[]);

            // Emit alt body
            builder.switch_to_block(alt_block);
            builder.seal_block(alt_block);
            ctx.declare_env(builder);

            // Bind pattern variables
            let mut bound_vars = Vec::new();
            for (i, &binder) in alt.binders.iter().enumerate() {
                let offset = CON_FIELDS_START + (8 * i as i32);
                let field_val =
                    builder
                        .ins()
                        .load(types::I64, MemFlags::trusted(), scrut_ptr, offset);
                builder.declare_value_needs_stack_map(field_val);
                ctx.env.insert(binder, SsaVal::HeapPtr(field_val));
                bound_vars.push(binder);
            }

            let result =
                ctx.emit_node(pipeline, builder, vmctx, gc_sig, oom_func, tree, alt.body)?;
            let result_ptr = ensure_heap_ptr(builder, vmctx, gc_sig, oom_func, result);
            builder.ins().jump(merge_block, &[result_ptr]);

            // Clean up
            for binder in bound_vars {
                ctx.env.remove(&binder);
            }

            // Continue to next check
            builder.switch_to_block(next_check_block);
            builder.seal_block(next_check_block);
        }
    }

    // Default or trap
    if let Some(alt) = default_alt {
        ctx.declare_env(builder);
        let result = ctx.emit_node(pipeline, builder, vmctx, gc_sig, oom_func, tree, alt.body)?;
        let result_ptr = ensure_heap_ptr(builder, vmctx, gc_sig, oom_func, result);
        builder.ins().jump(merge_block, &[result_ptr]);
    } else {
        emit_case_trap(pipeline, builder, scrut_ptr, data_alts)?;
    }

    Ok(())
}

/// Emit a call to `runtime_case_trap` instead of a bare `trap user2`.
/// Passes the scrutinee pointer and expected alt tags for diagnostic output.
fn emit_case_trap(
    pipeline: &mut CodegenPipeline,
    builder: &mut FunctionBuilder,
    scrut_ptr: Value,
    data_alts: &[&Alt<usize>],
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

    let trap_fn = pipeline
        .module
        .declare_function("runtime_case_trap", Linkage::Import, &{
            let mut sig = Signature::new(pipeline.isa.default_call_conv());
            sig.params.push(AbiParam::new(types::I64)); // scrut_ptr
            sig.params.push(AbiParam::new(types::I64)); // num_alts
            sig.params.push(AbiParam::new(types::I64)); // alt_tags
            sig.returns.push(AbiParam::new(types::I64)); // returns poison ptr
            sig
        })
        .map_err(|e| EmitError::CraneliftError(e.to_string()))?;
    let trap_ref = pipeline.module.declare_func_in_func(trap_fn, builder.func);
    let num_alts_val = builder.ins().iconst(types::I64, num_alts as i64);
    builder
        .ins()
        .call(trap_ref, &[scrut_ptr, num_alts_val, tags_addr]);
    builder.ins().trap(TrapCode::unwrap_user(2));
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn emit_lit_dispatch(
    ctx: &mut EmitContext,
    pipeline: &mut CodegenPipeline,
    builder: &mut FunctionBuilder,
    vmctx: Value,
    gc_sig: ir::SigRef,
    oom_func: ir::FuncRef,
    tree: &CoreExpr,
    scrut: SsaVal,
    lit_alts: &[&Alt<usize>],
    default_alt: Option<&Alt<usize>>,
    merge_block: ir::Block,
) -> Result<(), EmitError> {
    // Unbox scrutinee: Raw values are already unboxed, HeapPtr needs LIT_VALUE_OFFSET load
    let scrut_value = match scrut {
        SsaVal::Raw(v, _) => v,
        SsaVal::HeapPtr(ptr) => {
            builder
                .ins()
                .load(types::I64, MemFlags::trusted(), ptr, LIT_VALUE_OFFSET)
        }
    };

    for &alt in lit_alts {
        let alt_block = builder.create_block();
        let next_check_block = builder.create_block();

        if let AltCon::LitAlt(lit) = &alt.con {
            match lit {
                Literal::LitInt(n) => {
                    let lit_val = builder.ins().iconst(types::I64, *n);
                    let eq = builder.ins().icmp(IntCC::Equal, scrut_value, lit_val);
                    builder
                        .ins()
                        .brif(eq, alt_block, &[], next_check_block, &[]);
                }
                Literal::LitWord(n) => {
                    let lit_val = builder.ins().iconst(types::I64, *n as i64);
                    let eq = builder.ins().icmp(IntCC::Equal, scrut_value, lit_val);
                    builder
                        .ins()
                        .brif(eq, alt_block, &[], next_check_block, &[]);
                }
                Literal::LitChar(c) => {
                    let lit_val = builder.ins().iconst(types::I64, *c as i64);
                    let eq = builder.ins().icmp(IntCC::Equal, scrut_value, lit_val);
                    builder
                        .ins()
                        .brif(eq, alt_block, &[], next_check_block, &[]);
                }
                Literal::LitFloat(bits) => {
                    let scrut_f64 = builder.ins().bitcast(
                        types::F64,
                        MemFlags::new().with_endianness(ir::Endianness::Little),
                        scrut_value,
                    );
                    let lit_val = builder.ins().f64const(f64::from_bits(*bits));
                    let eq = builder
                        .ins()
                        .fcmp(ir::condcodes::FloatCC::Equal, scrut_f64, lit_val);
                    builder
                        .ins()
                        .brif(eq, alt_block, &[], next_check_block, &[]);
                }
                Literal::LitDouble(bits) => {
                    let scrut_f64 = builder.ins().bitcast(
                        types::F64,
                        MemFlags::new().with_endianness(ir::Endianness::Little),
                        scrut_value,
                    );
                    let lit_val = builder.ins().f64const(f64::from_bits(*bits));
                    let eq = builder
                        .ins()
                        .fcmp(ir::condcodes::FloatCC::Equal, scrut_f64, lit_val);
                    builder
                        .ins()
                        .brif(eq, alt_block, &[], next_check_block, &[]);
                }
                Literal::LitString(_) => {
                    return Err(EmitError::NotYetImplemented("LitString in Case".into()))
                }
            }
        }

        // Emit alt body
        builder.switch_to_block(alt_block);
        builder.seal_block(alt_block);
        ctx.declare_env(builder);
        let result = ctx.emit_node(pipeline, builder, vmctx, gc_sig, oom_func, tree, alt.body)?;
        let result_ptr = ensure_heap_ptr(builder, vmctx, gc_sig, oom_func, result);
        builder.ins().jump(merge_block, &[result_ptr]);

        // Continue to next check
        builder.switch_to_block(next_check_block);
        builder.seal_block(next_check_block);
    }

    // Default or trap
    if let Some(alt) = default_alt {
        ctx.declare_env(builder);
        let result = ctx.emit_node(pipeline, builder, vmctx, gc_sig, oom_func, tree, alt.body)?;
        let result_ptr = ensure_heap_ptr(builder, vmctx, gc_sig, oom_func, result);
        builder.ins().jump(merge_block, &[result_ptr]);
    } else {
        builder.ins().trap(TrapCode::unwrap_user(2));
    }

    Ok(())
}
