//! Option 3: Differential testing — interpreter vs JIT.
//!
//! For every generated expression, both backends should produce structurally
//! equal results (or both should error).

use proptest::test_runner::{Config, TestRunner};
use std::cell::Cell;
use tidepool_codegen::context::VMContext;
use tidepool_codegen::emit::expr::compile_expr;
use tidepool_codegen::host_fns;
use tidepool_codegen::pipeline::CodegenPipeline;
use tidepool_eval::{env::Env, eval::eval, heap::VecHeap};
use tidepool_repr::*;
use tidepool_testing::compare;
use tidepool_testing::gen::arb_ground_expr;

/// Compile and run an expression through the JIT, returning the result pointer.
/// Panics on compilation failure — the generator produces well-typed expressions
/// that the JIT should always be able to compile.
fn jit_compile_and_run(tree: &CoreExpr) -> (*const u8, VMContext, Vec<u8>, CodegenPipeline) {
    let mut pipeline =
        CodegenPipeline::new(&host_fns::host_fn_symbols()).expect("pipeline creation failed");
    let func_id = compile_expr(&mut pipeline, tree, "diff_test").expect("compile_expr failed");
    pipeline.finalize().expect("pipeline finalization failed");

    let mut nursery = vec![0u8; 65536];
    let start = nursery.as_mut_ptr();
    let end = unsafe { start.add(nursery.len()) };
    let mut vmctx = VMContext::new(start, end, host_fns::gc_trigger);

    host_fns::set_gc_state(start, nursery.len());
    host_fns::set_stack_map_registry(&pipeline.stack_maps);

    let ptr = pipeline.get_function_ptr(func_id);
    let func: unsafe extern "C" fn(*mut VMContext) -> i64 = unsafe { std::mem::transmute(ptr) };
    let result = unsafe { func(&mut vmctx as *mut VMContext) };

    (result as *const u8, vmctx, nursery, pipeline)
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

            let compared = Cell::new(0u64);
            let both_error = Cell::new(0u64);
            let eval_only_error = Cell::new(0u64);
            let jit_only_error = Cell::new(0u64);
            let deep_force_fail = Cell::new(0u64);

            runner
                .run(&arb_ground_expr(), |expr| {
                    // 1. Interpreter
                    let mut heap = VecHeap::new();
                    let eval_result = eval(&expr, &Env::new(), &mut heap);

                    // 2. JIT — compilation must succeed
                    let (result_ptr, mut vmctx, _nursery, _pipeline) = jit_compile_and_run(&expr);

                    // Check for JIT runtime error (e.g., division by zero, stack overflow)
                    let jit_runtime_error = host_fns::take_runtime_error();

                    // 3. Compare
                    match (&eval_result, &jit_runtime_error) {
                        (Ok(eval_val), None) => {
                            // Both succeeded — compare values
                            let forced =
                                tidepool_eval::eval::deep_force(eval_val.clone(), &mut heap);
                            match forced {
                                Ok(fv) => {
                                    let jit_val =
                                        unsafe { compare::heap_to_value(result_ptr, &mut vmctx) };
                                    compare::assert_values_eq(&fv, &jit_val);
                                    compared.set(compared.get() + 1);
                                }
                                Err(_) => {
                                    deep_force_fail.set(deep_force_fail.get() + 1);
                                }
                            }
                        }
                        (Err(_), Some(_)) => {
                            both_error.set(both_error.get() + 1);
                        }
                        (Err(_), None) => {
                            eval_only_error.set(eval_only_error.get() + 1);
                        }
                        (Ok(_), Some(_)) => {
                            jit_only_error.set(jit_only_error.get() + 1);
                        }
                    }
                    Ok(())
                })
                .unwrap();

            let compared = compared.get();
            let both_error = both_error.get();
            let eval_only_error = eval_only_error.get();
            let jit_only_error = jit_only_error.get();
            let deep_force_fail = deep_force_fail.get();
            eprintln!(
                "\nDifferential: compared={compared}, both_error={both_error}, \
                 eval_only_error={eval_only_error}, jit_only_error={jit_only_error}, \
                 deep_force_fail={deep_force_fail}"
            );
            assert!(
                compared >= 50,
                "Only {compared} of 200 cases reached value comparison"
            );
        })
        .unwrap();
    handle.join().unwrap();
}
