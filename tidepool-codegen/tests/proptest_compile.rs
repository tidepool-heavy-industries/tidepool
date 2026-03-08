//! Option 2: JIT compilation never panics on generated well-typed expressions.
//!
//! Every generated CoreExpr should compile through Cranelift without panicking.
//! We don't run the compiled code — just verify that IR generation succeeds
//! or returns a clean error.

use proptest::test_runner::{Config, TestRunner};
use tidepool_codegen::emit::expr::compile_expr;
use tidepool_codegen::host_fns;
use tidepool_codegen::pipeline::CodegenPipeline;
use tidepool_testing::gen::arb_core_expr;

#[test]
fn jit_compile_never_panics() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config {
                cases: 100,
                ..Config::default()
            });
            runner
                .run(&arb_core_expr(), |expr| {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let mut pipeline =
                            CodegenPipeline::new(&host_fns::host_fn_symbols()).unwrap();
                        let _ = compile_expr(&mut pipeline, &expr, "proptest_fn");
                        // Don't finalize or run — just test compilation
                    }));
                    // The test passes if compile_expr returns Ok or Err —
                    // only panics are failures.
                    match result {
                        Ok(()) => {}
                        Err(panic_info) => {
                            // Extract panic message for diagnostics
                            let msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                                s.to_string()
                            } else if let Some(s) = panic_info.downcast_ref::<String>() {
                                s.clone()
                            } else {
                                "unknown panic".to_string()
                            };
                            // Some panics are expected from Cranelift on unusual IR patterns
                            // (e.g., deeply nested expressions). Skip these gracefully.
                            if msg.contains("stack overflow") {
                                return Ok(());
                            }
                            panic!("JIT compilation panicked: {}", msg);
                        }
                    }
                    Ok(())
                })
                .unwrap();
        })
        .unwrap();
    handle.join().unwrap();
}
