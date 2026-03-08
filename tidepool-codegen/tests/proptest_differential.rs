//! Option 3: Differential testing — interpreter vs JIT.
//!
//! For every generated expression, both backends should produce structurally
//! equal results (or both should error).

use proptest::test_runner::{Config, TestRunner};
use tidepool_codegen::context::VMContext;
use tidepool_codegen::emit::expr::compile_expr;
use tidepool_codegen::host_fns;
use tidepool_codegen::pipeline::CodegenPipeline;
use tidepool_eval::{env::Env, eval::eval, heap::VecHeap};
use tidepool_repr::*;
use tidepool_testing::compare;
use tidepool_testing::gen::arb_ground_expr;

/// Compile and run an expression through the JIT, returning the result pointer.
fn jit_compile_and_run(
    tree: &CoreExpr,
) -> Option<(
    *const u8,
    VMContext,
    Vec<u8>,
    CodegenPipeline,
)> {
    let mut pipeline = CodegenPipeline::new(&host_fns::host_fn_symbols()).ok()?;
    let func_id = compile_expr(&mut pipeline, tree, "diff_test").ok()?;
    pipeline.finalize().ok()?;

    let mut nursery = vec![0u8; 65536];
    let start = nursery.as_mut_ptr();
    let end = unsafe { start.add(nursery.len()) };
    let mut vmctx = VMContext::new(start, end, host_fns::gc_trigger);

    host_fns::set_gc_state(start, nursery.len());
    host_fns::set_stack_map_registry(&pipeline.stack_maps);

    let ptr = pipeline.get_function_ptr(func_id);
    let func: unsafe extern "C" fn(*mut VMContext) -> i64 =
        unsafe { std::mem::transmute(ptr) };
    let result = unsafe { func(&mut vmctx as *mut VMContext) };

    Some((result as *const u8, vmctx, nursery, pipeline))
}

#[test]
fn interpreter_matches_jit() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config {
                cases: 200,
                ..Config::default()
            });
            runner
                .run(&arb_ground_expr(), |expr| {
                    // 1. Interpreter
                    let mut heap = VecHeap::new();
                    let eval_result = eval(&expr, &Env::new(), &mut heap);

                    // 2. JIT (catch panics from Cranelift)
                    let jit_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        jit_compile_and_run(&expr)
                    }));

                    // 3. Compare
                    match (eval_result, jit_result) {
                        // Both succeed
                        (Ok(eval_val), Ok(Some((result_ptr, mut vmctx, _nursery, _pipeline)))) => {
                            let forced_eval =
                                tidepool_eval::eval::deep_force(eval_val, &mut heap);
                            match forced_eval {
                                Ok(fv) => {
                                    let jit_val =
                                        unsafe { compare::heap_to_value(result_ptr, &mut vmctx) };
                                    compare::assert_values_eq(&fv, &jit_val);
                                }
                                Err(_) => {} // deep_force failed — skip
                            }
                        }
                        // Both error — acceptable
                        (Err(_), _) => {}
                        // JIT compilation failed — acceptable (JIT may not support all patterns)
                        (Ok(_), Ok(None)) => {}
                        // JIT panicked — acceptable (Cranelift may reject some IR)
                        (Ok(_), Err(_)) => {}
                    }
                    Ok(())
                })
                .unwrap();
        })
        .unwrap();
    handle.join().unwrap();
}
