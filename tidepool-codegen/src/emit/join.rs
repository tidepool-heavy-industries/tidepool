use crate::emit::expr::ensure_heap_ptr;
use crate::emit::*;
use crate::pipeline::CodegenPipeline;
use cranelift_codegen::ir::{self, types, BlockArg, InstBuilder, Value};
use cranelift_frontend::FunctionBuilder;
use tidepool_repr::*;

/// Emits a Join expression.
/// Join { label, params, rhs, body } creates a join point (a parameterized block)
/// that can be jumped to from within the body.
#[allow(clippy::too_many_arguments)]
pub fn emit_join(
    ctx: &mut EmitContext,
    pipeline: &mut CodegenPipeline,
    builder: &mut FunctionBuilder,
    vmctx: Value,
    gc_sig: ir::SigRef,
    oom_func: ir::FuncRef,
    tree: &CoreExpr,
    label: &JoinId,
    params: &[VarId],
    rhs_idx: usize,
    body_idx: usize,
) -> Result<SsaVal, EmitError> {
    // 1. Create a new block for the join point
    let join_block = builder.create_block();

    // 2. Add block params — one I64 param per join parameter
    for _ in params {
        builder.append_block_param(join_block, types::I64);
    }

    // 3. Create a continuation/merge block for the result
    let merge_block = builder.create_block();
    builder.append_block_param(merge_block, types::I64); // result

    // 4. Register the join point in ctx
    // We use a dummy Value(0) for param_types since Jump just needs to know they are heap pointers.
    let dummy_val = Value::from_u32(0);
    ctx.join_blocks.insert(
        *label,
        JoinInfo {
            block: join_block,
            param_types: params.iter().map(|_| SsaVal::HeapPtr(dummy_val)).collect(),
        },
    );

    // 5. Emit body (the continuation that may contain Jumps)
    let body_result = ctx.emit_node(pipeline, builder, vmctx, gc_sig, oom_func, tree, body_idx)?;
    let body_val = ensure_heap_ptr(builder, vmctx, gc_sig, oom_func, body_result);
    builder
        .ins()
        .jump(merge_block, &[BlockArg::Value(body_val)]);

    // 6. Switch to join block, emit rhs
    builder.switch_to_block(join_block);
    ctx.declare_env(builder);

    // Bind params to block params
    let block_params = builder.block_params(join_block).to_vec();
    let mut old_env_vals = Vec::new();
    for (i, param_var) in params.iter().enumerate() {
        let val = block_params[i];
        builder.declare_value_needs_stack_map(val); // CRITICAL
        let old_val = ctx.env.insert(*param_var, SsaVal::HeapPtr(val));
        old_env_vals.push((*param_var, old_val));
    }

    let rhs_result = ctx.emit_node(pipeline, builder, vmctx, gc_sig, oom_func, tree, rhs_idx)?;
    let rhs_val = ensure_heap_ptr(builder, vmctx, gc_sig, oom_func, rhs_result);
    builder.ins().jump(merge_block, &[BlockArg::Value(rhs_val)]);

    // 7. Seal blocks
    // Body is emitted, so all Jumps to join_block are known.
    builder.seal_block(join_block);
    // Both body and rhs paths to merge_block are known.
    builder.seal_block(merge_block);

    // 8. Switch to merge block, get result
    builder.switch_to_block(merge_block);
    let result = builder.block_params(merge_block)[0];
    builder.declare_value_needs_stack_map(result); // CRITICAL
    ctx.declare_env(builder);

    // 9. Clean up
    ctx.join_blocks.remove(label);
    for (param_var, old_val) in old_env_vals.into_iter().rev() {
        if let Some(v) = old_val {
            ctx.env.insert(param_var, v);
        } else {
            ctx.env.remove(&param_var);
        }
    }

    // 10. Return result
    Ok(SsaVal::HeapPtr(result))
}

/// Emits a Jump expression.
/// Jump { label, args } transfers control to the join point block.
#[allow(clippy::too_many_arguments)]
pub fn emit_jump(
    ctx: &mut EmitContext,
    pipeline: &mut CodegenPipeline,
    builder: &mut FunctionBuilder,
    vmctx: Value,
    gc_sig: ir::SigRef,
    oom_func: ir::FuncRef,
    tree: &CoreExpr,
    label: &JoinId,
    arg_indices: &[usize],
) -> Result<SsaVal, EmitError> {
    // 1. Look up label in ctx.join_blocks
    // Note: JoinInfo must be cloned or copied out because we'll be using the builder.
    // However, JoinInfo doesn't implement Clone. But Block and Value are Copy.
    // Actually, JoinInfo is not needed, just the block.
    let join_block = ctx
        .join_blocks
        .get(label)
        .ok_or_else(|| EmitError::NotYetImplemented(format!("Jump to unknown label {:?}", label)))?
        .block;

    // 2. Emit each arg
    let mut arg_values: Vec<BlockArg> = Vec::new();
    for &arg_idx in arg_indices {
        let val = ctx.emit_node(pipeline, builder, vmctx, gc_sig, oom_func, tree, arg_idx)?;
        // 3. Ensure all args are HeapPtr
        arg_values.push(BlockArg::Value(ensure_heap_ptr(
            builder, vmctx, gc_sig, oom_func, val,
        )));
    }

    // 4. Jump
    builder.ins().jump(join_block, &arg_values);

    // 5. After a jump, the current block is terminated.
    // Create a new unreachable block so Cranelift doesn't complain about instructions after a terminator.
    let unreachable_block = builder.create_block();
    builder.switch_to_block(unreachable_block);
    builder.seal_block(unreachable_block);

    // 6. Return a dummy SsaVal (dead code)
    Ok(SsaVal::Raw(
        builder.ins().iconst(types::I64, 0),
        LIT_TAG_INT,
    ))
}
