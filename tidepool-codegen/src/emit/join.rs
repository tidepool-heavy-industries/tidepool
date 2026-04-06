use crate::emit::expr::ensure_heap_ptr;
use crate::emit::*;
use cranelift_codegen::ir::{types, BlockArg, InstBuilder, Value};
use tidepool_repr::*;

/// Emits a Join expression.
/// Join { label, params, rhs, body } creates a join point (a parameterized block)
/// that can be jumped to from within the body.
pub fn emit_join(
    args: EmitArgs,
    label: &JoinId,
    params: &[VarId],
    rhs_idx: usize,
    body_idx: usize,
) -> Result<SsaVal, EmitError> {
    // 1. Create a new block for the join point
    let join_block = args.builder.create_block();

    // 2. Add block params — one I64 param per join parameter
    for _ in params {
        args.builder.append_block_param(join_block, types::I64);
    }

    // 3. Create a continuation/merge block for the result
    let merge_block = args.builder.create_block();
    args.builder.append_block_param(merge_block, types::I64); // result

    // 4. Register the join point in ctx
    // We use a dummy Value(0) for param_types since Jump just needs to know they are heap pointers.
    let dummy_val = Value::from_u32(0);
    args.ctx.join_blocks.register(
        *label,
        JoinInfo {
            block: join_block,
            param_types: params.iter().map(|_| SsaVal::HeapPtr(dummy_val)).collect(),
        },
    );

    // 5. Emit body (the continuation that may contain Jumps)
    let body_result = EmitContext::emit_node(
        EmitArgs {
            ctx: args.ctx,
            sess: args.sess,
            builder: args.builder,
            tail: args.tail,
        },
        body_idx,
    )?;
    let body_val = ensure_heap_ptr(
        args.builder,
        args.sess.vmctx,
        args.sess.gc_sig,
        args.sess.oom_func,
        body_result,
    );
    args.builder
        .ins()
        .jump(merge_block, &[BlockArg::Value(body_val)]);

    // 6. Switch to join block, emit rhs
    args.builder.switch_to_block(join_block);
    args.ctx.declare_env(args.builder);

    // Bind params to block params
    let block_params = args.builder.block_params(join_block).to_vec();
    let mut scope = EnvScope::new();
    // NOTE: EnvGuard cannot be used here because it would borrow ctx.env mutably,
    // preventing the use of ctx in emit_node.
    for (i, param_var) in params.iter().enumerate() {
        let val = block_params[i];
        args.builder.declare_value_needs_stack_map(val); // CRITICAL
        args.ctx
            .env
            .insert_scoped(&mut scope, *param_var, SsaVal::HeapPtr(val));
    }

    let rhs_result = EmitContext::emit_node(
        EmitArgs {
            ctx: args.ctx,
            sess: args.sess,
            builder: args.builder,
            tail: args.tail,
        },
        rhs_idx,
    )?;
    let rhs_val = ensure_heap_ptr(
        args.builder,
        args.sess.vmctx,
        args.sess.gc_sig,
        args.sess.oom_func,
        rhs_result,
    );
    args.builder
        .ins()
        .jump(merge_block, &[BlockArg::Value(rhs_val)]);

    // 7. Seal blocks
    // Body is emitted, so all Jumps to join_block are known.
    args.builder.seal_block(join_block);
    // Both body and rhs paths to merge_block are known.
    args.builder.seal_block(merge_block);

    // 8. Switch to merge block, get result
    args.builder.switch_to_block(merge_block);
    let result = args.builder.block_params(merge_block)[0];
    args.builder.declare_value_needs_stack_map(result); // CRITICAL
    args.ctx.declare_env(args.builder);

    // 9. Clean up
    args.ctx.join_blocks.remove(label);
    args.ctx.env.restore_scope(scope);

    // 10. Return result
    Ok(SsaVal::HeapPtr(result))
}

/// Emits a Jump expression.
/// Jump { label, args } transfers control to the join point block.
pub fn emit_jump(
    args: EmitArgs,
    label: &JoinId,
    arg_indices: &[usize],
) -> Result<SsaVal, EmitError> {
    // 1. Look up label in ctx.join_blocks
    let join_block = args.ctx.join_blocks.get(label)?.block;

    // 2. Emit each arg
    let mut arg_values: Vec<BlockArg> = Vec::new();
    for &arg_idx in arg_indices {
        // Jump arguments are always evaluated before we emit the jump terminator,
        // so they are not in tail position. Do NOT propagate any surrounding tail
        // context into these expressions: they must always be emitted as NonTail.
        let val = EmitContext::emit_node(
            EmitArgs {
                ctx: args.ctx,
                sess: args.sess,
                builder: args.builder,
                tail: TailCtx::NonTail,
            },
            arg_idx,
        )?;
        // 3. Ensure all args are HeapPtr
        arg_values.push(BlockArg::Value(ensure_heap_ptr(
            args.builder,
            args.sess.vmctx,
            args.sess.gc_sig,
            args.sess.oom_func,
            val,
        )));
    }

    // 4. Jump
    args.builder.ins().jump(join_block, &arg_values);

    // 5. After a jump, the current block is terminated.
    // Create a new unreachable block so Cranelift doesn't complain about instructions after a terminator.
    let unreachable_block = args.builder.create_block();
    args.builder.switch_to_block(unreachable_block);
    args.builder.seal_block(unreachable_block);

    // 6. Return a dummy SsaVal (dead code)
    Ok(SsaVal::Raw(
        args.builder.ins().iconst(types::I64, 0),
        LIT_TAG_INT,
    ))
}
