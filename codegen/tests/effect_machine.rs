use codegen::context::VMContext;
use codegen::pipeline::CodegenPipeline;
use codegen::host_fns;
use codegen::alloc::emit_alloc_fast_path;
use codegen::yield_type::{Yield, YieldError};
use codegen::effect_machine::CompiledEffectMachine;

use cranelift_codegen::ir::{self, types, AbiParam, InstBuilder, UserFuncName, MemFlags};
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};

const TAG_CON: u8 = 2;
const TAG_LIT: u8 = 3;

const VAL_CON_TAG: u64 = 1;
const E_CON_TAG: u64 = 2;
const UNION_CON_TAG: u64 = 3;
const LEAF_CON_TAG: u64 = 4;
const NODE_CON_TAG: u64 = 5;

/// Helper to build a JIT function for testing.
fn build_test_fn<F>(name: &str, build_body: F) -> (CodegenPipeline, unsafe extern "C" fn(*mut VMContext) -> *mut u8)
where
    F: FnOnce(&mut FunctionBuilder, ir::Value, ir::SigRef),
{
    let mut pipeline = CodegenPipeline::new(&host_fns::host_fn_symbols());
    let func_id = pipeline.declare_function(name);

    let mut ctx = Context::new();
    ctx.func = ir::Function::with_name_signature(UserFuncName::default(), pipeline.make_func_signature());
    let mut fb_ctx = FunctionBuilderContext::new();
    {
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut fb_ctx);
        let block = builder.create_block();
        builder.append_block_params_for_function_params(block);
        builder.switch_to_block(block);
        builder.seal_block(block);

        let vmctx = builder.block_params(block)[0];

        // Declare gc_trigger signature for the alloc slow path
        let mut gc_sig = ir::Signature::new(pipeline.isa.default_call_conv());
        gc_sig.params.push(AbiParam::new(types::I64));
        let gc_sig_ref = builder.import_signature(gc_sig);

        build_body(&mut builder, vmctx, gc_sig_ref);

        builder.finalize();
    }

    pipeline.define_function(func_id, &mut ctx);
    pipeline.finalize();

    let ptr = pipeline.get_function_ptr(func_id);
    let func = unsafe { std::mem::transmute(ptr) };
    (pipeline, func)
}

/// Helper to emit allocation of a LitInt object.
fn emit_alloc_lit_int(
    builder: &mut FunctionBuilder,
    vmctx: ir::Value,
    gc_sig: ir::SigRef,
    value: i64,
) -> ir::Value {
    let ptr = emit_alloc_fast_path(builder, vmctx, 24, gc_sig);
    let flags = MemFlags::trusted();

    // header(TAG_LIT=3, size=24)
    let tag_val = builder.ins().iconst(types::I64, TAG_LIT as i64);
    builder.ins().istore8(flags, tag_val, ptr, 0);
    let size_val = builder.ins().iconst(types::I64, 24);
    builder.ins().istore16(flags, size_val, ptr, 1);

    // lit_tag(0=Int)
    let lit_tag = builder.ins().iconst(types::I64, 0);
    builder.ins().istore8(flags, lit_tag, ptr, 8);

    // value(value)
    let val_const = builder.ins().iconst(types::I64, value);
    builder.ins().store(flags, val_const, ptr, 16);

    ptr
}

/// Helper to emit allocation of a LitWord object.
fn emit_alloc_lit_word(
    builder: &mut FunctionBuilder,
    vmctx: ir::Value,
    gc_sig: ir::SigRef,
    value: u64,
) -> ir::Value {
    let ptr = emit_alloc_fast_path(builder, vmctx, 24, gc_sig);
    let flags = MemFlags::trusted();

    // header(TAG_LIT=3, size=24)
    let tag_val = builder.ins().iconst(types::I64, TAG_LIT as i64);
    builder.ins().istore8(flags, tag_val, ptr, 0);
    let size_val = builder.ins().iconst(types::I64, 24);
    builder.ins().istore16(flags, size_val, ptr, 1);

    // lit_tag(1=Word)
    let lit_tag = builder.ins().iconst(types::I64, 1);
    builder.ins().istore8(flags, lit_tag, ptr, 8);

    // value(value)
    let val_const = builder.ins().iconst(types::I64, value as i64);
    builder.ins().store(flags, val_const, ptr, 16);

    ptr
}

/// Helper to emit allocation of a Con object with 1 field.
fn emit_alloc_con1(
    builder: &mut FunctionBuilder,
    vmctx: ir::Value,
    gc_sig: ir::SigRef,
    con_tag: u64,
    field0: ir::Value,
) -> ir::Value {
    let size = 24 + 8; // header + con_tag + num_fields + padding + field0
    let ptr = emit_alloc_fast_path(builder, vmctx, size as u64, gc_sig);
    let flags = MemFlags::trusted();

    // header(TAG_CON=2, size=32)
    let tag_val = builder.ins().iconst(types::I64, TAG_CON as i64);
    builder.ins().istore8(flags, tag_val, ptr, 0);
    let size_val = builder.ins().iconst(types::I64, size as i64);
    builder.ins().istore16(flags, size_val, ptr, 1);

    // con_tag
    let con_tag_val = builder.ins().iconst(types::I64, con_tag as i64);
    builder.ins().store(flags, con_tag_val, ptr, 8);

    // num_fields(1)
    let num_fields = builder.ins().iconst(types::I64, 1);
    builder.ins().istore16(flags, num_fields, ptr, 16);

    // field0
    builder.ins().store(flags, field0, ptr, 24);

    ptr
}

/// Helper to emit allocation of a Con object with 2 fields.
fn emit_alloc_con2(
    builder: &mut FunctionBuilder,
    vmctx: ir::Value,
    gc_sig: ir::SigRef,
    con_tag: u64,
    field0: ir::Value,
    field1: ir::Value,
) -> ir::Value {
    let size = 24 + 16; // header + con_tag + num_fields + padding + field0 + field1
    let ptr = emit_alloc_fast_path(builder, vmctx, size as u64, gc_sig);
    let flags = MemFlags::trusted();

    // header(TAG_CON=2, size=40)
    let tag_val = builder.ins().iconst(types::I64, TAG_CON as i64);
    builder.ins().istore8(flags, tag_val, ptr, 0);
    let size_val = builder.ins().iconst(types::I64, size as i64);
    builder.ins().istore16(flags, size_val, ptr, 1);

    // con_tag
    let con_tag_val = builder.ins().iconst(types::I64, con_tag as i64);
    builder.ins().store(flags, con_tag_val, ptr, 8);

    // num_fields(2)
    let num_fields = builder.ins().iconst(types::I64, 2);
    builder.ins().istore16(flags, num_fields, ptr, 16);

    // field0
    builder.ins().store(flags, field0, ptr, 24);
    // field1
    builder.ins().store(flags, field1, ptr, 32);

    ptr
}

/// Test 1: Yield::Done from Val result.
#[test]
fn test_yield_done_val() {
    let (_pipeline, func) = build_test_fn("test_val", |builder, vmctx, gc_sig| {
        let lit_ptr = emit_alloc_lit_int(builder, vmctx, gc_sig, 42);
        let val_ptr = emit_alloc_con1(builder, vmctx, gc_sig, VAL_CON_TAG, lit_ptr);
        builder.ins().return_(&[val_ptr]);
    });

    let mut nursery = vec![0u8; 4096];
    let start = nursery.as_mut_ptr();
    let end = unsafe { start.add(4096) };
    let vmctx = VMContext::new(start, end, host_fns::gc_trigger);

    let mut machine = CompiledEffectMachine::new(
        func,
        vmctx,
        codegen::effect_machine::ConTags {
            val: VAL_CON_TAG,
            e: E_CON_TAG,
            union: UNION_CON_TAG,
            leaf: LEAF_CON_TAG,
            node: NODE_CON_TAG,
        },
    );
    let result = machine.step();

    match result {
        Yield::Done(val_ptr) => {
            // val_ptr should point to the Lit(42) object
            let tag = unsafe { *val_ptr };
            assert_eq!(tag, TAG_LIT);
            let val = unsafe { *(val_ptr.add(16) as *const i64) };
            assert_eq!(val, 42);
        }
        _ => panic!("Expected Yield::Done, got {:?}", result),
    }
}

/// Test 2: Yield::Request from E result.
#[test]
fn test_yield_request_e() {
    let (_pipeline, func) = build_test_fn("test_e", |builder, vmctx, gc_sig| {
        let request_lit = emit_alloc_lit_int(builder, vmctx, gc_sig, 99);
        let tag_word = emit_alloc_lit_word(builder, vmctx, gc_sig, 7); // Tag value 7
        let union_ptr = emit_alloc_con2(builder, vmctx, gc_sig, UNION_CON_TAG, tag_word, request_lit);
        
        // Placeholder for continuation
        let cont_ptr = emit_alloc_lit_int(builder, vmctx, gc_sig, 0);
        
        let e_ptr = emit_alloc_con2(builder, vmctx, gc_sig, E_CON_TAG, union_ptr, cont_ptr);
        builder.ins().return_(&[e_ptr]);
    });

    let mut nursery = vec![0u8; 4096];
    let start = nursery.as_mut_ptr();
    let end = unsafe { start.add(4096) };
    let vmctx = VMContext::new(start, end, host_fns::gc_trigger);

    let mut machine = CompiledEffectMachine::new(
        func,
        vmctx,
        codegen::effect_machine::ConTags {
            val: VAL_CON_TAG,
            e: E_CON_TAG,
            union: UNION_CON_TAG,
            leaf: LEAF_CON_TAG,
            node: NODE_CON_TAG,
        },
    );
    let result = machine.step();

    match result {
        Yield::Request { tag, request, continuation } => {
            assert_eq!(tag, 7);
            
            // Verify request is Lit(99)
            let req_tag = unsafe { *request };
            assert_eq!(req_tag, TAG_LIT);
            let req_val = unsafe { *(request.add(16) as *const i64) };
            assert_eq!(req_val, 99);
            
            // Verify continuation is there
            assert!(!continuation.is_null());
        }
        _ => panic!("Expected Yield::Request, got {:?}", result),
    }
}

/// Test 3: CompiledEffectMachine is Send.
#[test]
fn test_machine_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<CompiledEffectMachine>();
}

/// Test 4: Unexpected tag → YieldError.
#[test]
fn test_unexpected_tag() {
    let (_pipeline, func) = build_test_fn("test_lit_result", |builder, vmctx, gc_sig| {
        let lit_ptr = emit_alloc_lit_int(builder, vmctx, gc_sig, 42);
        builder.ins().return_(&[lit_ptr]);
    });

    let mut nursery = vec![0u8; 4096];
    let start = nursery.as_mut_ptr();
    let end = unsafe { start.add(4096) };
    let vmctx = VMContext::new(start, end, host_fns::gc_trigger);

    let mut machine = CompiledEffectMachine::new(
        func,
        vmctx,
        codegen::effect_machine::ConTags {
            val: VAL_CON_TAG,
            e: E_CON_TAG,
            union: UNION_CON_TAG,
            leaf: LEAF_CON_TAG,
            node: NODE_CON_TAG,
        },
    );
    let result = machine.step();

    assert_eq!(result, Yield::Error(YieldError::UnexpectedTag(TAG_LIT)));
}

/// Test 5: runtime_error(0) → YieldError::DivisionByZero.
#[test]
fn test_runtime_error_div_zero() {
    let (_pipeline, func) = build_test_fn("test_div_zero", |builder, vmctx, gc_sig| {
        // Call runtime_error(0) which sets thread-local and returns null
        let mut err_sig = ir::Signature::new(builder.func.signature.call_conv);
        err_sig.params.push(AbiParam::new(types::I64));
        err_sig.returns.push(AbiParam::new(types::I64));
        let err_sig_ref = builder.import_signature(err_sig);

        let err_ptr = builder.ins().iconst(types::I64, host_fns::runtime_error as *const u8 as i64);
        let kind = builder.ins().iconst(types::I64, 0); // divZero
        let inst = builder.ins().call_indirect(err_sig_ref, err_ptr, &[kind]);
        let result = builder.inst_results(inst)[0];
        builder.ins().return_(&[result]);
    });

    let mut nursery = vec![0u8; 4096];
    let start = nursery.as_mut_ptr();
    let end = unsafe { start.add(4096) };
    let vmctx = VMContext::new(start, end, host_fns::gc_trigger);
    host_fns::reset_test_counters();

    let mut machine = CompiledEffectMachine::new(
        func,
        vmctx,
        codegen::effect_machine::ConTags {
            val: VAL_CON_TAG,
            e: E_CON_TAG,
            union: UNION_CON_TAG,
            leaf: LEAF_CON_TAG,
            node: NODE_CON_TAG,
        },
    );
    let result = machine.step();
    assert_eq!(result, Yield::Error(YieldError::DivisionByZero));
}

/// Test 6: runtime_error(1) → YieldError::Overflow.
#[test]
fn test_runtime_error_overflow() {
    let (_pipeline, func) = build_test_fn("test_overflow", |builder, vmctx, gc_sig| {
        let mut err_sig = ir::Signature::new(builder.func.signature.call_conv);
        err_sig.params.push(AbiParam::new(types::I64));
        err_sig.returns.push(AbiParam::new(types::I64));
        let err_sig_ref = builder.import_signature(err_sig);

        let err_ptr = builder.ins().iconst(types::I64, host_fns::runtime_error as *const u8 as i64);
        let kind = builder.ins().iconst(types::I64, 1); // overflow
        let inst = builder.ins().call_indirect(err_sig_ref, err_ptr, &[kind]);
        let result = builder.inst_results(inst)[0];
        builder.ins().return_(&[result]);
    });

    let mut nursery = vec![0u8; 4096];
    let start = nursery.as_mut_ptr();
    let end = unsafe { start.add(4096) };
    let vmctx = VMContext::new(start, end, host_fns::gc_trigger);
    host_fns::reset_test_counters();

    let mut machine = CompiledEffectMachine::new(
        func,
        vmctx,
        codegen::effect_machine::ConTags {
            val: VAL_CON_TAG,
            e: E_CON_TAG,
            union: UNION_CON_TAG,
            leaf: LEAF_CON_TAG,
            node: NODE_CON_TAG,
        },
    );
    let result = machine.step();
    assert_eq!(result, Yield::Error(YieldError::Overflow));
}

/// Test 7: null without runtime_error → YieldError::NullPointer (not a false positive).
#[test]
fn test_null_without_runtime_error() {
    let (_pipeline, func) = build_test_fn("test_null", |builder, vmctx, gc_sig| {
        let null = builder.ins().iconst(types::I64, 0);
        builder.ins().return_(&[null]);
    });

    let mut nursery = vec![0u8; 4096];
    let start = nursery.as_mut_ptr();
    let end = unsafe { start.add(4096) };
    let vmctx = VMContext::new(start, end, host_fns::gc_trigger);
    host_fns::reset_test_counters();

    let mut machine = CompiledEffectMachine::new(
        func,
        vmctx,
        codegen::effect_machine::ConTags {
            val: VAL_CON_TAG,
            e: E_CON_TAG,
            union: UNION_CON_TAG,
            leaf: LEAF_CON_TAG,
            node: NODE_CON_TAG,
        },
    );
    let result = machine.step();
    assert_eq!(result, Yield::Error(YieldError::NullPointer));
}

/// Test 8: Unknown con_tag → YieldError.
#[test]
fn test_unexpected_con_tag() {
    let (_pipeline, func) = build_test_fn("test_unknown_con", |builder, vmctx, gc_sig| {
        let dummy_field = emit_alloc_lit_int(builder, vmctx, gc_sig, 0);
        let con_ptr = emit_alloc_con1(builder, vmctx, gc_sig, 999, dummy_field);
        builder.ins().return_(&[con_ptr]);
    });

    let mut nursery = vec![0u8; 4096];
    let start = nursery.as_mut_ptr();
    let end = unsafe { start.add(4096) };
    let vmctx = VMContext::new(start, end, host_fns::gc_trigger);

    let mut machine = CompiledEffectMachine::new(
        func,
        vmctx,
        codegen::effect_machine::ConTags {
            val: VAL_CON_TAG,
            e: E_CON_TAG,
            union: UNION_CON_TAG,
            leaf: LEAF_CON_TAG,
            node: NODE_CON_TAG,
        },
    );
    let result = machine.step();

    assert_eq!(result, Yield::Error(YieldError::UnexpectedConTag(999)));
}