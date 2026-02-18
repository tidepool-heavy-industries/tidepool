use cranelift_codegen::ir::{self, types, InstBuilder, Value, MemFlags};
use cranelift_frontend::FunctionBuilder;

/// Offset of alloc_ptr within VMContext (byte 0).
const VMCTX_ALLOC_PTR_OFFSET: i32 = 0;
/// Offset of alloc_limit within VMContext (byte 8).
const VMCTX_ALLOC_LIMIT_OFFSET: i32 = 8;
/// Offset of gc_trigger within VMContext (byte 16).
const VMCTX_GC_TRIGGER_OFFSET: i32 = 16;

/// Emit the alloc fast-path as inline Cranelift IR.
///
/// This is a bump-pointer allocation:
/// 1. Load alloc_ptr from VMContext
/// 2. new_ptr = alloc_ptr + size (8-byte aligned)
/// 3. If new_ptr > alloc_limit: call gc_trigger (cold path), then retry
/// 4. Store new_ptr as alloc_ptr
/// 5. Return old alloc_ptr (start of allocated region)
///
/// `vmctx_val` is the SSA value holding the VMContext pointer.
/// `size` is the number of bytes to allocate (will be rounded up to 8-byte alignment).
/// `gc_trigger_sig` is the signature reference for the gc_trigger call.
///
/// Returns the SSA value pointing to the start of the allocated memory.
pub fn emit_alloc_fast_path(
    builder: &mut FunctionBuilder,
    vmctx_val: Value,
    size: u64,
    gc_trigger_sig: ir::SigRef,
) -> Value {
    let aligned_size = (size + 7) & !7;
    let flags = MemFlags::trusted();

    // Load current alloc_ptr
    let alloc_ptr = builder.ins().load(types::I64, flags, vmctx_val, VMCTX_ALLOC_PTR_OFFSET);

    // Compute new_ptr = alloc_ptr + aligned_size
    let size_val = builder.ins().iconst(types::I64, aligned_size as i64);
    let new_ptr = builder.ins().iadd(alloc_ptr, size_val);

    // Load alloc_limit
    let alloc_limit = builder.ins().load(types::I64, flags, vmctx_val, VMCTX_ALLOC_LIMIT_OFFSET);

    // Compare: new_ptr > alloc_limit
    let overflow = builder.ins().icmp(ir::condcodes::IntCC::UnsignedGreaterThan, new_ptr, alloc_limit);

    let slow_block = builder.create_block();
    let fast_store_block = builder.create_block();
    let continue_block = builder.create_block();
    builder.append_block_param(continue_block, types::I64); // result ptr

    builder.ins().brif(overflow, slow_block, &[], fast_store_block, &[]);

    // --- Fast path: store new_ptr, jump to continue with old alloc_ptr ---
    builder.switch_to_block(fast_store_block);
    builder.seal_block(fast_store_block);
    builder.ins().store(flags, new_ptr, vmctx_val, VMCTX_ALLOC_PTR_OFFSET);
    builder.ins().jump(continue_block, &[alloc_ptr]);

    // --- Slow path: call gc_trigger, retry alloc ---
    builder.switch_to_block(slow_block);
    builder.seal_block(slow_block);

    let gc_trigger_ptr = builder.ins().load(types::I64, flags, vmctx_val, VMCTX_GC_TRIGGER_OFFSET);
    builder.ins().call_indirect(gc_trigger_sig, gc_trigger_ptr, &[vmctx_val]);

    // After GC: reload alloc_ptr, bump, store, continue
    let post_gc_ptr = builder.ins().load(types::I64, flags, vmctx_val, VMCTX_ALLOC_PTR_OFFSET);
    let post_gc_new = builder.ins().iadd(post_gc_ptr, size_val);
    builder.ins().store(flags, post_gc_new, vmctx_val, VMCTX_ALLOC_PTR_OFFSET);
    builder.ins().jump(continue_block, &[post_gc_ptr]);

    // --- Continue: result is the old alloc_ptr from whichever path ---
    builder.switch_to_block(continue_block);
    builder.seal_block(continue_block);

    builder.block_params(continue_block)[0]
}
