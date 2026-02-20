use tidepool_codegen::context::VMContext;
use tidepool_codegen::pipeline::CodegenPipeline;
use tidepool_codegen::host_fns;
use tidepool_codegen::gc::frame_walker;

use cranelift_codegen::ir::{self, types, UserFuncName, InstBuilder};
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::Module;

#[test]
fn test_frame_walker_finds_roots() {
    let mut pipeline = CodegenPipeline::new(&host_fns::host_fn_symbols());

    // Declare gc_trigger as import
    let gc_sig_ext = {
        let mut sig = ir::Signature::new(pipeline.isa.default_call_conv());
        sig.params.push(ir::AbiParam::new(types::I64));
        sig
    };
    let gc_id = pipeline.module.declare_function("gc_trigger", cranelift_module::Linkage::Import, &gc_sig_ext).unwrap();

    let func_id = pipeline.declare_function("test_find_roots");
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

        // Two "heap pointers" with known values
        let val1 = 0xAAAA_BBBB_CCCC_DDDDu64;
        let val2 = 0x1111_2222_3333_4444u64;

        let ptr1 = builder.ins().iconst(types::I64, val1 as i64);
        builder.declare_value_needs_stack_map(ptr1);
        let ptr2 = builder.ins().iconst(types::I64, val2 as i64);
        builder.declare_value_needs_stack_map(ptr2);

        // Call gc_trigger (safepoint — both ptrs must be in stack map)
        let gc_ref = pipeline.module.declare_func_in_func(gc_id, builder.func);
        builder.ins().call(gc_ref, &[vmctx]);

        // Use both ptrs after the call to keep them live
        let sum = builder.ins().iadd(ptr1, ptr2);
        builder.ins().return_(&[sum]);
        builder.finalize();
    }

    pipeline.define_function(func_id, &mut ctx);
    pipeline.finalize();

    host_fns::reset_test_counters();
    host_fns::set_stack_map_registry(&pipeline.stack_maps);

    let mut nursery = vec![0u8; 4096];
    let start = nursery.as_mut_ptr();
    let end = unsafe { start.add(4096) };
    let mut vmctx = VMContext::new(start, end, host_fns::gc_trigger);

    let f_ptr = pipeline.get_function_ptr(func_id);
    let f: unsafe extern "C" fn(*mut VMContext) -> i64 = unsafe { std::mem::transmute(f_ptr) };
    let _result = unsafe { f(&mut vmctx as *mut VMContext) };

    assert_eq!(host_fns::gc_trigger_call_count(), 1);
    
    let roots = host_fns::last_gc_roots();
    assert_eq!(roots.len(), 2, "Should have found 2 roots");

    let values: Vec<u64> = roots.iter().map(|r| r.heap_ptr as u64).collect();
    assert!(values.contains(&0xAAAA_BBBB_CCCC_DDDDu64));
    assert!(values.contains(&0x1111_2222_3333_4444u64));

    host_fns::clear_stack_map_registry();
}

#[test]
fn test_frame_walker_rewrite_roots() {
    let mut pipeline = CodegenPipeline::new(&host_fns::host_fn_symbols());

    // Declare gc_trigger as import
    let gc_sig_ext = {
        let mut sig = ir::Signature::new(pipeline.isa.default_call_conv());
        sig.params.push(ir::AbiParam::new(types::I64));
        sig
    };
    let gc_id = pipeline.module.declare_function("gc_trigger", cranelift_module::Linkage::Import, &gc_sig_ext).unwrap();

    let func_id = pipeline.declare_function("test_rewrite_roots");
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

        // Known value
        let val = 0x1234_5678_9ABC_DEF0u64;
        let ptr = builder.ins().iconst(types::I64, val as i64);
        builder.declare_value_needs_stack_map(ptr);

        // Call gc_trigger
        let gc_ref = pipeline.module.declare_func_in_func(gc_id, builder.func);
        builder.ins().call(gc_ref, &[vmctx]);

        // Return the pointer value (it might have been rewritten!)
        builder.ins().return_(&[ptr]);
        builder.finalize();
    }

    pipeline.define_function(func_id, &mut ctx);
    pipeline.finalize();

    host_fns::reset_test_counters();
    host_fns::set_stack_map_registry(&pipeline.stack_maps);
    host_fns::set_gc_test_hook(|roots| {
        unsafe {
            frame_walker::rewrite_roots(roots, &|ptr| {
                if ptr as u64 == 0x1234_5678_9ABC_DEF0u64 {
                    0xFEED_FACE_CAFE_BEEFu64 as *mut u8
                } else {
                    ptr
                }
            });
        }
    });

    let mut nursery = vec![0u8; 4096];
    let start = nursery.as_mut_ptr();
    let end = unsafe { start.add(4096) };
    let mut vmctx = VMContext::new(start, end, host_fns::gc_trigger);

    let f: unsafe extern "C" fn(*mut VMContext) -> i64 = unsafe { std::mem::transmute(pipeline.get_function_ptr(func_id)) };
    let result = unsafe { f(&mut vmctx as *mut VMContext) };

    assert_eq!(result as u64, 0xFEED_FACE_CAFE_BEEFu64, "Root should have been rewritten");

    host_fns::clear_gc_test_hook();
    host_fns::clear_stack_map_registry();
}

#[test]
fn test_frame_walker_terminates_at_jit_boundary() {
    let mut pipeline = CodegenPipeline::new(&host_fns::host_fn_symbols());
    let func_id = pipeline.declare_function("test_boundary");

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

        // Declare gc_trigger signature
        let mut gc_sig = ir::Signature::new(pipeline.isa.default_call_conv());
        gc_sig.params.push(ir::AbiParam::new(types::I64));
        let gc_id = pipeline.module.declare_function("gc_trigger", cranelift_module::Linkage::Import, &gc_sig).unwrap();
        let gc_ref = pipeline.module.declare_func_in_func(gc_id, builder.func);

        builder.ins().call(gc_ref, &[vmctx]);

        let val = builder.ins().iconst(types::I64, 42);
        builder.ins().return_(&[val]);
        builder.finalize();
    }

    pipeline.define_function(func_id, &mut ctx);
    pipeline.finalize();

    host_fns::reset_test_counters();
    host_fns::set_stack_map_registry(&pipeline.stack_maps);

    let mut nursery = vec![0u8; 4096];
    let start = nursery.as_mut_ptr();
    let end = unsafe { start.add(4096) };
    let mut vmctx = VMContext::new(start, end, host_fns::gc_trigger);

    let f: unsafe extern "C" fn(*mut VMContext) -> i64 = unsafe { std::mem::transmute(pipeline.get_function_ptr(func_id)) };
    
    // This call goes Rust -> JIT -> Rust (gc_trigger)
    // The frame walker should see the JIT frame but stop at the Rust frame.
    unsafe { f(&mut vmctx as *mut VMContext) };

    assert_eq!(host_fns::gc_trigger_call_count(), 1);
    
    // If it didn't terminate, it would likely crash or return many bogus roots.
    // The current JIT frame doesn't have any roots declared.
    let roots = host_fns::last_gc_roots();
    assert_eq!(roots.len(), 0);

    host_fns::clear_stack_map_registry();
}
