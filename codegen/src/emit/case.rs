use crate::pipeline::CodegenPipeline;
use crate::emit::*;
use crate::emit::expr::ensure_heap_ptr;
use core_repr::{VarId, Alt, AltCon, Literal, CoreExpr};
use cranelift_codegen::ir::{self, types, InstBuilder, MemFlags, Value, condcodes::IntCC, JumpTableData, TrapCode, BlockCall};
use cranelift_frontend::FunctionBuilder;
use std::collections::HashMap;

/// Emit Case dispatch.
pub fn emit_case(
    ctx: &mut EmitContext,
    pipeline: &mut CodegenPipeline,
    builder: &mut FunctionBuilder,
    vmctx: Value,
    gc_sig: ir::SigRef,
    tree: &CoreExpr,
    scrutinee_idx: usize,
    binder: &VarId,
    alts: &[Alt<usize>],
) -> Result<SsaVal, EmitError> {
    // 1. Emit scrutinee
    let scrut = ctx.emit_node(pipeline, builder, vmctx, gc_sig, tree, scrutinee_idx)?;
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
            ctx, pipeline, builder, vmctx, gc_sig, tree, scrut_ptr, &data_alts, default_alt, merge_block,
        )?;
    } else if !lit_alts.is_empty() {
        emit_lit_dispatch(
            ctx, pipeline, builder, vmctx, gc_sig, tree, scrut_ptr, &lit_alts, default_alt, merge_block,
        )?;
    } else if let Some(alt) = default_alt {
        // Default only
        let result = ctx.emit_node(pipeline, builder, vmctx, gc_sig, tree, alt.body)?;
        let result_ptr = ensure_heap_ptr(builder, vmctx, gc_sig, result);
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

    // 6. Clean up case binder
    ctx.env.remove(binder);

    Ok(SsaVal::HeapPtr(result))
}

fn emit_data_dispatch(
    ctx: &mut EmitContext,
    pipeline: &mut CodegenPipeline,
    builder: &mut FunctionBuilder,
    vmctx: Value,
    gc_sig: ir::SigRef,
    tree: &CoreExpr,
    scrut_ptr: Value,
    data_alts: &[&Alt<usize>],
    default_alt: Option<&Alt<usize>>,
    merge_block: ir::Block,
) -> Result<(), EmitError> {
    // Load con_tag: offset 8
    let con_tag = builder.ins().load(types::I64, MemFlags::trusted(), scrut_ptr, CON_TAG_OFFSET);
    let con_tag_i32 = builder.ins().ireduce(types::I32, con_tag);

    let mut max_tag = 0;
    let mut tag_to_alt = HashMap::new();
    for &alt in data_alts {
        if let AltCon::DataAlt(tag) = &alt.con {
            tag_to_alt.insert(tag.0, alt);
            if tag.0 > max_tag {
                max_tag = tag.0;
            }
        }
    }

    let default_block = builder.create_block();
    let mut alt_blocks = HashMap::new();
    let mut table = Vec::new();

    for tag_idx in 0..=max_tag {
        if let Some(_alt) = tag_to_alt.get(&tag_idx) {
            let block = *alt_blocks.entry(tag_idx).or_insert_with(|| builder.create_block());
            table.push(BlockCall::new(block, &[], &mut builder.func.dfg.value_lists));
        } else {
            table.push(BlockCall::new(default_block, &[], &mut builder.func.dfg.value_lists));
        }
    }

    let jt_data = JumpTableData::new(BlockCall::new(default_block, &[], &mut builder.func.dfg.value_lists), &table);
    let jt = builder.create_jump_table(jt_data);
    builder.ins().br_table(con_tag_i32, jt);

    // Emit DataAlt blocks
    for (&tag_idx, &alt) in &tag_to_alt {
        let block = alt_blocks[&tag_idx];
        builder.switch_to_block(block);
        builder.seal_block(block);

        // Bind pattern variables
        let mut bound_vars = Vec::new();
        for (i, &binder) in alt.binders.iter().enumerate() {
            let offset = CON_FIELDS_START + (8 * i as i32);
            let field_val = builder.ins().load(types::I64, MemFlags::trusted(), scrut_ptr, offset);
            builder.declare_value_needs_stack_map(field_val);
            ctx.env.insert(binder, SsaVal::HeapPtr(field_val));
            bound_vars.push(binder);
        }

        let result = ctx.emit_node(pipeline, builder, vmctx, gc_sig, tree, alt.body)?;
        let result_ptr = ensure_heap_ptr(builder, vmctx, gc_sig, result);
        builder.ins().jump(merge_block, &[result_ptr]);

        // Clean up
        for binder in bound_vars {
            ctx.env.remove(&binder);
        }
    }

    // Emit default block
    builder.switch_to_block(default_block);
    builder.seal_block(default_block);
    if let Some(alt) = default_alt {
        let result = ctx.emit_node(pipeline, builder, vmctx, gc_sig, tree, alt.body)?;
        let result_ptr = ensure_heap_ptr(builder, vmctx, gc_sig, result);
        builder.ins().jump(merge_block, &[result_ptr]);
    } else {
        builder.ins().trap(TrapCode::unwrap_user(2));
    }

    Ok(())
}

fn emit_lit_dispatch(
    ctx: &mut EmitContext,
    pipeline: &mut CodegenPipeline,
    builder: &mut FunctionBuilder,
    vmctx: Value,
    gc_sig: ir::SigRef,
    tree: &CoreExpr,
    scrut_ptr: Value,
    lit_alts: &[&Alt<usize>],
    default_alt: Option<&Alt<usize>>,
    merge_block: ir::Block,
) -> Result<(), EmitError> {
    // Load value from LIT object: offset 16
    let scrut_value = builder.ins().load(types::I64, MemFlags::trusted(), scrut_ptr, LIT_VALUE_OFFSET);

    for &alt in lit_alts {
        let alt_block = builder.create_block();
        let next_check_block = builder.create_block();

        if let AltCon::LitAlt(lit) = &alt.con {
            match lit {
                Literal::LitInt(n) => {
                    let lit_val = builder.ins().iconst(types::I64, *n);
                    let eq = builder.ins().icmp(IntCC::Equal, scrut_value, lit_val);
                    builder.ins().brif(eq, alt_block, &[], next_check_block, &[]);
                }
                Literal::LitWord(n) => {
                    let lit_val = builder.ins().iconst(types::I64, *n as i64);
                    let eq = builder.ins().icmp(IntCC::Equal, scrut_value, lit_val);
                    builder.ins().brif(eq, alt_block, &[], next_check_block, &[]);
                }
                Literal::LitChar(c) => {
                    let lit_val = builder.ins().iconst(types::I64, *c as i64);
                    let eq = builder.ins().icmp(IntCC::Equal, scrut_value, lit_val);
                    builder.ins().brif(eq, alt_block, &[], next_check_block, &[]);
                }
                Literal::LitFloat(bits) => {
                    let scrut_f64 = builder.ins().bitcast(types::F64, MemFlags::new().with_endianness(ir::Endianness::Little), scrut_value);
                    let lit_val = builder.ins().f64const(f64::from_bits(*bits));
                    let eq = builder.ins().fcmp(ir::condcodes::FloatCC::Equal, scrut_f64, lit_val);
                    builder.ins().brif(eq, alt_block, &[], next_check_block, &[]);
                }
                Literal::LitDouble(bits) => {
                    let scrut_f64 = builder.ins().bitcast(types::F64, MemFlags::new().with_endianness(ir::Endianness::Little), scrut_value);
                    let lit_val = builder.ins().f64const(f64::from_bits(*bits));
                    let eq = builder.ins().fcmp(ir::condcodes::FloatCC::Equal, scrut_f64, lit_val);
                    builder.ins().brif(eq, alt_block, &[], next_check_block, &[]);
                }
                Literal::LitString(_) => return Err(EmitError::NotYetImplemented("LitString in Case".into())),
            }
        }

        // Emit alt body
        builder.switch_to_block(alt_block);
        builder.seal_block(alt_block);
        let result = ctx.emit_node(pipeline, builder, vmctx, gc_sig, tree, alt.body)?;
        let result_ptr = ensure_heap_ptr(builder, vmctx, gc_sig, result);
        builder.ins().jump(merge_block, &[result_ptr]);

        // Continue to next check
        builder.switch_to_block(next_check_block);
        builder.seal_block(next_check_block);
    }

    // Default or trap
    if let Some(alt) = default_alt {
        let result = ctx.emit_node(pipeline, builder, vmctx, gc_sig, tree, alt.body)?;
        let result_ptr = ensure_heap_ptr(builder, vmctx, gc_sig, result);
        builder.ins().jump(merge_block, &[result_ptr]);
    } else {
        builder.ins().trap(TrapCode::unwrap_user(2));
    }

    Ok(())
}
