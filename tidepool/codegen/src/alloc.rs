use cranelift_codegen::ir::{self, types, BlockArg, InstBuilder, MemFlags, Value};
use cranelift_frontend::FunctionBuilder;
use tidepool_heap::execution_descriptor::ObjectDescriptor;

use crate::layout::*;

/// Failure does not publish payload components. Cranelift nevertheless needs
/// ABI-shaped returns; callers branch on status before observing any payload.
pub(crate) fn emit_prepared_failure_return(builder: &mut FunctionBuilder<'_>, status: Value) {
    let payload_types: Vec<_> = builder
        .func
        .signature
        .returns
        .iter()
        .skip(1)
        .map(|parameter| parameter.value_type)
        .collect();
    let mut values = vec![status];
    for ty in payload_types {
        let value = if ty == types::F32 {
            builder.ins().f32const(0.0)
        } else if ty == types::F64 {
            builder.ins().f64const(0.0)
        } else {
            builder.ins().iconst(ty, 0)
        };
        values.push(value);
    }
    builder.ins().return_(&values);
}
/// Emit a prepared-object allocation against the installed nursery.
///
/// The slow path is a status-returning host call rather than the legacy
/// poison-pointer path.  This helper is consequently only suitable for an
/// entry whose ABI returns [`crate::prepared_control::CallStatus`]. The caller
/// has checked that the descriptor has eight-byte alignment and extent.
pub fn emit_prepared_alloc_fast_path(
    builder: &mut FunctionBuilder,
    vmctx_val: Value,
    descriptor: &ObjectDescriptor,
    gc_trigger: ir::FuncRef,
) -> Value {
    let extent = u64::from(descriptor.allocation_extent());
    debug_assert_eq!(descriptor.allocation_alignment(), 8);
    debug_assert!(descriptor.allocation_extent() >= 16 && extent % 8 == 0);
    emit_prepared_reserve_fast_path(builder, vmctx_val, gc_trigger, extent)
}

/// Reserve one contiguous prepared-nursery region. All callers must derive
/// every object address after this call: its slow path may collect and rewrite
/// live managed SSA values before it returns.
pub fn emit_prepared_reserve_fast_path(
    builder: &mut FunctionBuilder,
    vmctx_val: Value,
    gc_trigger: ir::FuncRef,
    extent: u64,
) -> Value {
    debug_assert!(extent >= 16 && extent.is_multiple_of(8));
    let flags = MemFlags::trusted();
    let extent_val = builder.ins().iconst(types::I64, extent as i64);

    let slow_block = builder.create_block();
    let fast_store_block = builder.create_block();
    let continue_block = builder.create_block();
    builder.append_block_param(continue_block, types::I64);

    let alloc_ptr = builder
        .ins()
        .load(types::I64, flags, vmctx_val, VMCTX_ALLOC_PTR_OFFSET);
    let alloc_limit = builder
        .ins()
        .load(types::I64, flags, vmctx_val, VMCTX_ALLOC_LIMIT_OFFSET);
    let new_ptr = builder.ins().iadd(alloc_ptr, extent_val);
    let wrapped = builder
        .ins()
        .icmp(ir::condcodes::IntCC::UnsignedLessThan, new_ptr, alloc_ptr);
    let exceeds_limit = builder.ins().icmp(
        ir::condcodes::IntCC::UnsignedGreaterThan,
        new_ptr,
        alloc_limit,
    );
    let needs_gc = builder.ins().bor(wrapped, exceeds_limit);
    builder
        .ins()
        .brif(needs_gc, slow_block, &[], fast_store_block, &[]);

    builder.switch_to_block(fast_store_block);
    builder.seal_block(fast_store_block);
    builder
        .ins()
        .store(flags, new_ptr, vmctx_val, VMCTX_ALLOC_PTR_OFFSET);
    builder
        .ins()
        .jump(continue_block, &[BlockArg::Value(alloc_ptr)]);

    builder.switch_to_block(slow_block);
    builder.seal_block(slow_block);
    let reserve = builder.ins().iconst(types::I64, extent as i64);
    let call = builder.ins().call(gc_trigger, &[vmctx_val, reserve]);
    let status = builder.inst_results(call)[0];
    let success = builder.ins().iconst(
        types::I32,
        crate::prepared_control::CallStatus::Success as i64,
    );
    let succeeded = builder
        .ins()
        .icmp(ir::condcodes::IntCC::Equal, status, success);
    let retry_block = builder.create_block();
    let failed_block = builder.create_block();
    builder
        .ins()
        .brif(succeeded, retry_block, &[], failed_block, &[]);

    builder.switch_to_block(failed_block);
    builder.seal_block(failed_block);
    emit_prepared_failure_return(builder, status);

    builder.switch_to_block(retry_block);
    builder.seal_block(retry_block);
    let post_gc_ptr = builder
        .ins()
        .load(types::I64, flags, vmctx_val, VMCTX_ALLOC_PTR_OFFSET);
    let post_gc_limit = builder
        .ins()
        .load(types::I64, flags, vmctx_val, VMCTX_ALLOC_LIMIT_OFFSET);
    let post_gc_new = builder.ins().iadd(post_gc_ptr, extent_val);
    let post_gc_wrapped = builder.ins().icmp(
        ir::condcodes::IntCC::UnsignedLessThan,
        post_gc_new,
        post_gc_ptr,
    );
    let post_gc_exceeds_limit = builder.ins().icmp(
        ir::condcodes::IntCC::UnsignedGreaterThan,
        post_gc_new,
        post_gc_limit,
    );
    let post_gc_failed = builder.ins().bor(post_gc_wrapped, post_gc_exceeds_limit);
    let retry_store_block = builder.create_block();
    let exhausted_block = builder.create_block();
    builder
        .ins()
        .brif(post_gc_failed, exhausted_block, &[], retry_store_block, &[]);

    builder.switch_to_block(exhausted_block);
    builder.seal_block(exhausted_block);
    let exhausted = builder.ins().iconst(
        types::I32,
        crate::prepared_control::CallStatus::IntegrityFailure as i64,
    );
    emit_prepared_failure_return(builder, exhausted);

    builder.switch_to_block(retry_store_block);
    builder.seal_block(retry_store_block);
    builder
        .ins()
        .store(flags, post_gc_new, vmctx_val, VMCTX_ALLOC_PTR_OFFSET);
    builder
        .ins()
        .jump(continue_block, &[BlockArg::Value(post_gc_ptr)]);

    builder.switch_to_block(continue_block);
    builder.seal_block(continue_block);
    builder.block_params(continue_block)[0]
}
