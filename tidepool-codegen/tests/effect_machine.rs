use tidepool_codegen::alloc::emit_alloc_fast_path;
use tidepool_codegen::context::VMContext;
use tidepool_codegen::effect_machine::{CompiledEffectMachine, ConTags};
use tidepool_codegen::emit::expr::compile_expr;
use tidepool_codegen::host_fns;
use tidepool_codegen::pipeline::CodegenPipeline;
use tidepool_codegen::yield_type::{Yield, YieldError};
use tidepool_heap::layout;
use tidepool_repr::{CoreExpr, CoreFrame, DataConId, VarId};

use cranelift_codegen::ir::{self, types, AbiParam, InstBuilder, MemFlags, UserFuncName};
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::Module;

const TAG_CON: u8 = 2;
const TAG_LIT: u8 = 3;

const VAL_CON_TAG: u64 = 1;
const E_CON_TAG: u64 = 2;
const UNION_CON_TAG: u64 = 3;
const LEAF_CON_TAG: u64 = 4;
const NODE_CON_TAG: u64 = 5;

/// Helper to build a JIT function for testing.
fn build_test_fn<F>(
    name: &str,
    build_body: F,
) -> (
    CodegenPipeline,
    unsafe extern "C" fn(*mut VMContext) -> *mut u8,
)
where
    F: FnOnce(&mut FunctionBuilder, ir::Value, ir::SigRef, ir::FuncRef),
{
    let mut pipeline = CodegenPipeline::new(&host_fns::host_fn_symbols()).unwrap();
    let func_id = pipeline.declare_function(name).expect("failed to declare");

    let mut ctx = Context::new();
    ctx.func =
        ir::Function::with_name_signature(UserFuncName::default(), pipeline.make_func_signature());
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

        let oom_func = {
            let mut sig = ir::Signature::new(pipeline.isa.default_call_conv());
            sig.returns.push(AbiParam::new(types::I64));
            let func_id = pipeline
                .module
                .declare_function("runtime_oom", cranelift_module::Linkage::Import, &sig)
                .unwrap();
            pipeline
                .module
                .declare_func_in_func(func_id, &mut builder.func)
        };

        build_body(&mut builder, vmctx, gc_sig_ref, oom_func);

        builder.finalize();
    }

    pipeline
        .define_function(func_id, &mut ctx)
        .expect("failed to define");
    pipeline.finalize().expect("failed to finalize");

    let ptr = pipeline.get_function_ptr(func_id);
    let func: unsafe extern "C" fn(*mut VMContext) -> *mut u8 = unsafe {
        std::mem::transmute::<*const u8, unsafe extern "C" fn(*mut VMContext) -> *mut u8>(ptr)
    };
    (pipeline, func)
}

/// Helper to emit allocation of a LitInt object.
fn emit_alloc_lit_int(
    builder: &mut FunctionBuilder,
    vmctx: ir::Value,
    gc_sig: ir::SigRef,
    oom_func: ir::FuncRef,
    value: i64,
) -> ir::Value {
    let ptr = emit_alloc_fast_path(builder, vmctx, 24, gc_sig, oom_func);
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
    oom_func: ir::FuncRef,
    value: u64,
) -> ir::Value {
    let ptr = emit_alloc_fast_path(builder, vmctx, 24, gc_sig, oom_func);
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
    oom_func: ir::FuncRef,
    con_tag: u64,
    field0: ir::Value,
) -> ir::Value {
    let size = 24 + 8; // header + con_tag + num_fields + padding + field0
    let ptr = emit_alloc_fast_path(builder, vmctx, size as u64, gc_sig, oom_func);
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
    oom_func: ir::FuncRef,
    con_tag: u64,
    field0: ir::Value,
    field1: ir::Value,
) -> ir::Value {
    let size = 24 + 16; // header + con_tag + num_fields + padding + field0 + field1
    let ptr = emit_alloc_fast_path(builder, vmctx, size as u64, gc_sig, oom_func);
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
    let (_pipeline, func) = build_test_fn("test_val", |builder, vmctx, gc_sig, oom_func| {
        let lit_ptr = emit_alloc_lit_int(builder, vmctx, gc_sig, oom_func, 42);
        let val_ptr = emit_alloc_con1(builder, vmctx, gc_sig, oom_func, VAL_CON_TAG, lit_ptr);
        builder.ins().return_(&[val_ptr]);
    });

    let mut nursery = vec![0u8; 4096];
    let start = nursery.as_mut_ptr();
    let end = unsafe { start.add(4096) };
    let vmctx = VMContext::new(start, end, host_fns::gc_trigger);

    let mut machine = CompiledEffectMachine::new(
        func,
        vmctx,
        tidepool_codegen::effect_machine::ConTags {
            val: VAL_CON_TAG,
            e: E_CON_TAG,
            union: UNION_CON_TAG,
            leaf: LEAF_CON_TAG,
            node: NODE_CON_TAG,
        },
    );
    let result = machine.step();

    let Yield::Done(val_ptr) = result else {
        panic!("Expected Yield::Done, got {:?}", result);
    };
    // val_ptr should point to the Lit(42) object
    let tag = unsafe { *val_ptr };
    assert_eq!(tag, TAG_LIT);
    let val = unsafe { *(val_ptr.add(16) as *const i64) };
    assert_eq!(val, 42);
}

/// Test 2: Yield::Request from E result.
#[test]
fn test_yield_request_e() {
    let (_pipeline, func) = build_test_fn("test_e", |builder, vmctx, gc_sig, oom_func| {
        let request_lit = emit_alloc_lit_int(builder, vmctx, gc_sig, oom_func, 99);
        let tag_word = emit_alloc_lit_word(builder, vmctx, gc_sig, oom_func, 7); // Tag value 7
        let union_ptr = emit_alloc_con2(
            builder,
            vmctx,
            gc_sig,
            oom_func,
            UNION_CON_TAG,
            tag_word,
            request_lit,
        );

        // Placeholder for continuation
        let cont_ptr = emit_alloc_lit_int(builder, vmctx, gc_sig, oom_func, 0);

        let e_ptr = emit_alloc_con2(
            builder, vmctx, gc_sig, oom_func, E_CON_TAG, union_ptr, cont_ptr,
        );
        builder.ins().return_(&[e_ptr]);
    });

    let mut nursery = vec![0u8; 4096];
    let start = nursery.as_mut_ptr();
    let end = unsafe { start.add(4096) };
    let vmctx = VMContext::new(start, end, host_fns::gc_trigger);

    let mut machine = CompiledEffectMachine::new(
        func,
        vmctx,
        tidepool_codegen::effect_machine::ConTags {
            val: VAL_CON_TAG,
            e: E_CON_TAG,
            union: UNION_CON_TAG,
            leaf: LEAF_CON_TAG,
            node: NODE_CON_TAG,
        },
    );
    let result = machine.step();

    let Yield::Request {
        tag,
        request,
        continuation,
    } = result
    else {
        panic!("Expected Yield::Request, got {:?}", result);
    };
    assert_eq!(tag, 7);

    // Verify request is Lit(99)
    let req_tag = unsafe { *request };
    assert_eq!(req_tag, TAG_LIT);
    let req_val = unsafe { *(request.add(16) as *const i64) };
    assert_eq!(req_val, 99);

    // Verify continuation is there
    assert!(!continuation.is_null());
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
    let (_pipeline, func) = build_test_fn("test_lit_result", |builder, vmctx, gc_sig, oom_func| {
        let lit_ptr = emit_alloc_lit_int(builder, vmctx, gc_sig, oom_func, 42);
        builder.ins().return_(&[lit_ptr]);
    });

    let mut nursery = vec![0u8; 4096];
    let start = nursery.as_mut_ptr();
    let end = unsafe { start.add(4096) };
    let vmctx = VMContext::new(start, end, host_fns::gc_trigger);

    let mut machine = CompiledEffectMachine::new(
        func,
        vmctx,
        tidepool_codegen::effect_machine::ConTags {
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
    let (_pipeline, func) =
        build_test_fn("test_div_zero", |builder, _vmctx, _gc_sig, _oom_func| {
            // Call runtime_error(0) which sets thread-local and returns null
            let mut err_sig = ir::Signature::new(builder.func.signature.call_conv);
            err_sig.params.push(AbiParam::new(types::I64));
            err_sig.returns.push(AbiParam::new(types::I64));
            let err_sig_ref = builder.import_signature(err_sig);

            let err_ptr = builder
                .ins()
                .iconst(types::I64, host_fns::runtime_error as *const u8 as i64);
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
        tidepool_codegen::effect_machine::ConTags {
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
    let (_pipeline, func) =
        build_test_fn("test_overflow", |builder, _vmctx, _gc_sig, _oom_func| {
            let mut err_sig = ir::Signature::new(builder.func.signature.call_conv);
            err_sig.params.push(AbiParam::new(types::I64));
            err_sig.returns.push(AbiParam::new(types::I64));
            let err_sig_ref = builder.import_signature(err_sig);

            let err_ptr = builder
                .ins()
                .iconst(types::I64, host_fns::runtime_error as *const u8 as i64);
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
        tidepool_codegen::effect_machine::ConTags {
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
    let (_pipeline, func) = build_test_fn("test_null", |builder, _vmctx, _gc_sig, _oom_func| {
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
        tidepool_codegen::effect_machine::ConTags {
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
    let (_pipeline, func) =
        build_test_fn("test_unknown_con", |builder, vmctx, gc_sig, oom_func| {
            let dummy_field = emit_alloc_lit_int(builder, vmctx, gc_sig, oom_func, 0);
            let con_ptr = emit_alloc_con1(builder, vmctx, gc_sig, oom_func, 999, dummy_field);
            builder.ins().return_(&[con_ptr]);
        });

    let mut nursery = vec![0u8; 4096];
    let start = nursery.as_mut_ptr();
    let end = unsafe { start.add(4096) };
    let vmctx = VMContext::new(start, end, host_fns::gc_trigger);

    let mut machine = CompiledEffectMachine::new(
        func,
        vmctx,
        tidepool_codegen::effect_machine::ConTags {
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

/// Dummy function for tests that only use machine.resume.
unsafe extern "C" fn dummy_machine_func(_vmctx: *mut VMContext) -> *mut u8 {
    std::ptr::null_mut()
}

fn create_machine(vmctx: VMContext) -> CompiledEffectMachine {
    CompiledEffectMachine::new(
        dummy_machine_func,
        vmctx,
        ConTags {
            val: VAL_CON_TAG,

            e: E_CON_TAG,

            union: UNION_CON_TAG,

            leaf: LEAF_CON_TAG,

            node: NODE_CON_TAG,
        },
    )
}

/// Helper to allocate Con objects in tests since machine.alloc_con is private.
unsafe fn alloc_con_heap(
    machine: &mut CompiledEffectMachine,
    con_tag: u64,
    fields: &[*mut u8],
) -> *mut u8 {
    let size = 24 + 8 * fields.len();

    let ptr = tidepool_codegen::heap_bridge::bump_alloc_from_vmctx(machine.vmctx_mut(), size);

    if ptr.is_null() {
        return std::ptr::null_mut();
    }

    layout::write_header(ptr, layout::TAG_CON, size as u16);

    *(ptr.add(layout::CON_TAG_OFFSET) as *mut u64) = con_tag;

    *(ptr.add(layout::CON_NUM_FIELDS_OFFSET) as *mut u16) = fields.len() as u16;

    for (i, &fp) in fields.iter().enumerate() {
        *(ptr.add(layout::CON_FIELDS_OFFSET + 8 * i) as *mut *mut u8) = fp;
    }

    ptr
}

/// Test 9: resume(Leaf(f), x) -> f(x)

#[test]

fn test_resume_leaf_identity() {
    let mut pipeline = CodegenPipeline::new(&host_fns::host_fn_symbols()).unwrap();

    // Identity closure: \x -> Val(x)

    let x = VarId(1);

    let val_con_id = DataConId(VAL_CON_TAG);

    let tree = CoreExpr {
        nodes: vec![
            CoreFrame::Var(x), // 0
            CoreFrame::Con {
                tag: val_con_id,
                fields: vec![0],
            }, // 1: Val(x)
            CoreFrame::Lam { binder: x, body: 1 }, // 2: \x -> Val(x)
        ],
    };

    let func_id = compile_expr(&mut pipeline, &tree, "identity").unwrap();

    pipeline.finalize().unwrap();

    let func_ptr = pipeline.get_function_ptr(func_id);

    let func: unsafe extern "C" fn(*mut VMContext) -> *mut u8 =
        unsafe { std::mem::transmute(func_ptr) };

    let mut nursery = vec![0u8; 4096];

    let start = nursery.as_mut_ptr();

    let end = unsafe { start.add(4096) };

    let vmctx = VMContext::new(start, end, host_fns::gc_trigger);

    let mut machine = create_machine(vmctx);

    // Get closure ptr by running identity func

    let closure_ptr = unsafe { func(machine.vmctx_mut()) };

    assert!(!closure_ptr.is_null());

    // Build Leaf(closure)

    let leaf_ptr = unsafe { alloc_con_heap(&mut machine, LEAF_CON_TAG, &[closure_ptr]) };

    // Build Lit(42) as argument

    // Actually let's use a real Lit if we can, but a Val(null) is fine for identity.

    // Wait, let's use a Lit to be sure.

    let lit_ptr = unsafe {
        let size = 24;

        let p = tidepool_codegen::heap_bridge::bump_alloc_from_vmctx(machine.vmctx_mut(), size);

        layout::write_header(p, layout::TAG_LIT, size as u16);

        *p.add(layout::LIT_TAG_OFFSET) = 0; // Int

        *(p.add(layout::LIT_VALUE_OFFSET) as *mut i64) = 42;

        p
    };

    let result = unsafe { machine.resume(leaf_ptr, lit_ptr) };

    let Yield::Done(res_ptr) = result else {
        panic!("Expected Yield::Done, got {:?}", result);
    };
    assert_eq!(res_ptr, lit_ptr);

    let val = unsafe { *(res_ptr.add(16) as *const i64) };

    assert_eq!(val, 42);
}

/// Test 10: resume(Node(Leaf(f), Leaf(g)), x) -> g(f(x))

#[test]

fn test_resume_node_identity() {
    let mut pipeline = CodegenPipeline::new(&host_fns::host_fn_symbols()).unwrap();

    // Identity closure: \x -> Val(x)

    let x = VarId(1);

    let val_con_id = DataConId(VAL_CON_TAG);

    let tree = CoreExpr {
        nodes: vec![
            CoreFrame::Var(x), // 0
            CoreFrame::Con {
                tag: val_con_id,
                fields: vec![0],
            }, // 1: Val(x)
            CoreFrame::Lam { binder: x, body: 1 }, // 2: \x -> Val(x)
        ],
    };

    let func_id = compile_expr(&mut pipeline, &tree, "identity").unwrap();

    pipeline.finalize().unwrap();

    let func_ptr = pipeline.get_function_ptr(func_id);

    let func: unsafe extern "C" fn(*mut VMContext) -> *mut u8 =
        unsafe { std::mem::transmute(func_ptr) };

    let mut nursery = vec![0u8; 8192]; // Bigger nursery for more allocs

    let start = nursery.as_mut_ptr();

    let end = unsafe { start.add(8192) };

    let vmctx = VMContext::new(start, end, host_fns::gc_trigger);

    let mut machine = create_machine(vmctx);

    let closure_ptr = unsafe { func(machine.vmctx_mut()) };

    // Leaf(f) and Leaf(g)

    let leaf_f = unsafe { alloc_con_heap(&mut machine, LEAF_CON_TAG, &[closure_ptr]) };

    let leaf_g = unsafe { alloc_con_heap(&mut machine, LEAF_CON_TAG, &[closure_ptr]) };

    // Node(Leaf(f), Leaf(g))

    let node_ptr = unsafe { alloc_con_heap(&mut machine, NODE_CON_TAG, &[leaf_f, leaf_g]) };

    let lit_ptr = unsafe {
        let size = 24;

        let p = tidepool_codegen::heap_bridge::bump_alloc_from_vmctx(machine.vmctx_mut(), size);

        layout::write_header(p, layout::TAG_LIT, size as u16);

        *p.add(layout::LIT_TAG_OFFSET) = 0; // Int

        *(p.add(layout::LIT_VALUE_OFFSET) as *mut i64) = 100;

        p
    };

    let result = unsafe { machine.resume(node_ptr, lit_ptr) };

    let Yield::Done(res_ptr) = result else {
        panic!("Expected Yield::Done, got {:?}", result);
    };
    assert_eq!(res_ptr, lit_ptr);

    let val = unsafe { *(res_ptr.add(16) as *const i64) };

    assert_eq!(val, 100);
}

/// Test 11: resume(null, x) -> Error(NullPointer)

#[test]

fn test_resume_null_continuation() {
    let mut nursery = vec![0u8; 1024];

    let vmctx = VMContext::new(
        nursery.as_mut_ptr(),
        unsafe { nursery.as_mut_ptr().add(1024) },
        host_fns::gc_trigger,
    );

    let mut machine = create_machine(vmctx);

    let result = unsafe { machine.resume(std::ptr::null_mut(), std::ptr::null_mut()) };

    assert_eq!(result, Yield::Error(YieldError::NullPointer));
}

/// Test 12: resume(Lit(0), x) -> Error(NullPointer) (because apply_cont returns null on unknown tag)

#[test]

fn test_resume_unknown_tag() {
    let mut nursery = vec![0u8; 1024];

    let vmctx = VMContext::new(
        nursery.as_mut_ptr(),
        unsafe { nursery.as_mut_ptr().add(1024) },
        host_fns::gc_trigger,
    );

    let mut machine = create_machine(vmctx);

    let lit_ptr = unsafe {
        let size = 24;

        let p = tidepool_codegen::heap_bridge::bump_alloc_from_vmctx(machine.vmctx_mut(), size);

        layout::write_header(p, layout::TAG_LIT, size as u16);

        p
    };

    let result = unsafe { machine.resume(lit_ptr, std::ptr::null_mut()) };

    assert_eq!(result, Yield::Error(YieldError::NullPointer));
}

/// Test 13: Node(Leaf(f), Leaf(g)) where f(x) returns Request E

#[test]

fn test_resume_node_with_effect_result() {
    let mut pipeline = CodegenPipeline::new(&host_fns::host_fn_symbols()).unwrap();

    // f = \x -> E(Union(7, x), Leaf(identity))

    let x = VarId(1);

    let val_con_id = DataConId(VAL_CON_TAG);

    let e_con_id = DataConId(E_CON_TAG);

    let union_con_id = DataConId(UNION_CON_TAG);

    let leaf_con_id = DataConId(LEAF_CON_TAG);

    let tree_identity = CoreExpr {
        nodes: vec![
            CoreFrame::Var(x), // 0
            CoreFrame::Con {
                tag: val_con_id,
                fields: vec![0],
            }, // 1: Val(x)
            CoreFrame::Lam { binder: x, body: 1 }, // 2: \x -> Val(x)
        ],
    };

    // Identity to use as g

    let func_id_identity = compile_expr(&mut pipeline, &tree_identity, "identity").unwrap();

    pipeline.finalize().unwrap();

    let id_func_ptr = pipeline.get_function_ptr(func_id_identity);

    let id_func: unsafe extern "C" fn(*mut VMContext) -> *mut u8 =
        unsafe { std::mem::transmute(id_func_ptr) };

    let mut nursery = vec![0u8; 8192];

    let start = nursery.as_mut_ptr();

    let end = unsafe { start.add(8192) };

    let vmctx = VMContext::new(start, end, host_fns::gc_trigger);

    let mut machine = create_machine(vmctx);

    let id_closure_ptr = unsafe { id_func(machine.vmctx_mut()) };

    let leaf_id = unsafe { alloc_con_heap(&mut machine, LEAF_CON_TAG, &[id_closure_ptr]) };

    // Closure g: identity

    let leaf_g = leaf_id;

    // k1 = identity, but we'll cheat and make resume call it.

    // If k1(x) returns E(union, k_prime), then Node(k1, k2) returns E(union, Node(k_prime, k2))

    // To test this, we need k1 to return E.

    // We can compile a function that returns E.

    let tree_returns_e = CoreExpr {
        nodes: vec![
            CoreFrame::Var(x),                                 // 0
            CoreFrame::Lit(tidepool_repr::Literal::LitInt(0)), // 1
            CoreFrame::Con {
                tag: leaf_con_id,
                fields: vec![1],
            }, // 2: Leaf(0)
            CoreFrame::Lit(tidepool_repr::Literal::LitWord(7)), // 3
            CoreFrame::Con {
                tag: union_con_id,
                fields: vec![3, 0],
            }, // 4: Union(7, x)
            CoreFrame::Con {
                tag: e_con_id,
                fields: vec![4, 2],
            }, // 5: E(...)
            CoreFrame::Lam { binder: x, body: 5 },             // 6
        ],
    };

    let mut pipeline2 = CodegenPipeline::new(&host_fns::host_fn_symbols()).unwrap();

    let f_id = compile_expr(&mut pipeline2, &tree_returns_e, "returns_e").unwrap();

    pipeline2.finalize().unwrap();

    let f_func_ptr = pipeline2.get_function_ptr(f_id);

    let f_func: unsafe extern "C" fn(*mut VMContext) -> *mut u8 =
        unsafe { std::mem::transmute(f_func_ptr) };

    let f_closure = unsafe { f_func(machine.vmctx_mut()) };

    let leaf_f = unsafe { alloc_con_heap(&mut machine, LEAF_CON_TAG, &[f_closure]) };

    let node_ptr = unsafe { alloc_con_heap(&mut machine, NODE_CON_TAG, &[leaf_f, leaf_g]) };

    let lit_ptr = unsafe {
        let size = 24;

        let p = tidepool_codegen::heap_bridge::bump_alloc_from_vmctx(machine.vmctx_mut(), size);

        layout::write_header(p, layout::TAG_LIT, size as u16);

        *p.add(layout::LIT_TAG_OFFSET) = 0; // Int

        *(p.add(layout::LIT_VALUE_OFFSET) as *mut i64) = 123;

        p
    };

    let result = unsafe { machine.resume(node_ptr, lit_ptr) };

    let Yield::Request {
        tag,
        request,
        continuation,
    } = result
    else {
        panic!("Expected Yield::Request, got {:?}", result);
    };
    assert_eq!(tag, 7);

    assert_eq!(request, lit_ptr);

    // continuation should be Node(k_prime, leaf_g)

    let tag = unsafe { *continuation };

    assert_eq!(tag, layout::TAG_CON);

    let con_tag = unsafe { *(continuation.add(layout::CON_TAG_OFFSET) as *const u64) };

    assert_eq!(con_tag, NODE_CON_TAG);

    let k_prime = unsafe { *(continuation.add(layout::CON_FIELDS_OFFSET) as *const *mut u8) };

    let k2 = unsafe { *(continuation.add(layout::CON_FIELDS_OFFSET + 8) as *const *mut u8) };

    assert_eq!(k2, leaf_g);

    assert!(!k_prime.is_null());
}
