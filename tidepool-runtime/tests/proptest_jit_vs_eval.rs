use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};
use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_optimize::pipeline::optimize;
use tidepool_testing::gen::arb_core_expr;
use tidepool_testing::proptest::{build_table_for_expr, check_jit_vs_eval, values_equal};

#[test]
fn jit_agrees_with_eval() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            // `source_file` makes a failure PERSIST to
            // proptest_jit_vs_eval.proptest-regressions — without it,
            // `TestRunner::new` (unlike the proptest! macro) cannot write
            // one at all, which is how jit_small_nursery_agrees's two
            // 2026-08-23 in-shard failures left no repro behind
            // (scratchpad review-nursery-flake.md).
            let mut runner = TestRunner::new(Config {
                cases: 50,
                source_file: Some(file!()),
                ..Config::default()
            });
            runner
                .run(&arb_core_expr(), |expr| check_jit_vs_eval(expr, 64 * 1024))
                .unwrap();
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn jit_agrees_with_eval_after_optimize() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            // `source_file` makes a failure PERSIST to
            // proptest_jit_vs_eval.proptest-regressions — without it,
            // `TestRunner::new` (unlike the proptest! macro) cannot write
            // one at all, which is how jit_small_nursery_agrees's two
            // 2026-08-23 in-shard failures left no repro behind
            // (scratchpad review-nursery-flake.md).
            let mut runner = TestRunner::new(Config {
                cases: 50,
                source_file: Some(file!()),
                ..Config::default()
            });
            runner
                .run(&arb_core_expr(), |mut expr| {
                    optimize(&mut expr).unwrap();
                    check_jit_vs_eval(expr, 64 * 1024)
                })
                .unwrap();
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn jit_small_nursery_agrees() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            // `source_file` makes a failure PERSIST to
            // proptest_jit_vs_eval.proptest-regressions — without it,
            // `TestRunner::new` (unlike the proptest! macro) cannot write
            // one at all, which is how jit_small_nursery_agrees's two
            // 2026-08-23 in-shard failures left no repro behind
            // (scratchpad review-nursery-flake.md).
            let mut runner = TestRunner::new(Config {
                cases: 50,
                source_file: Some(file!()),
                ..Config::default()
            });
            runner
                .run(&arb_core_expr(), |expr| {
                    // Tiny 4KB nursery to force GC
                    check_jit_vs_eval(expr, 4 * 1024)
                })
                .unwrap();
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn jit_deterministic() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            // `source_file` makes a failure PERSIST to
            // proptest_jit_vs_eval.proptest-regressions — without it,
            // `TestRunner::new` (unlike the proptest! macro) cannot write
            // one at all, which is how jit_small_nursery_agrees's two
            // 2026-08-23 in-shard failures left no repro behind
            // (scratchpad review-nursery-flake.md).
            let mut runner = TestRunner::new(Config {
                cases: 50,
                source_file: Some(file!()),
                ..Config::default()
            });
            runner
                .run(&arb_core_expr(), |expr| {
                    let table = build_table_for_expr(&expr);
                    let nursery_size = 64 * 1024;

                    let res1 = match JitEffectMachine::compile(&expr, &table, nursery_size) {
                        Ok(mut m) => m.run_pure().ok(),
                        Err(_) => None,
                    };

                    let res2 = match JitEffectMachine::compile(&expr, &table, nursery_size) {
                        Ok(mut m) => m.run_pure().ok(),
                        Err(_) => None,
                    };

                    if let (Some(v1), Some(v2)) = (res1, res2) {
                        prop_assert!(
                            values_equal(&v1, &v2),
                            "JIT results are not deterministic.
Run 1: {:?}
Run 2: {:?}
Expr: {:#?}",
                            v1,
                            v2,
                            expr
                        );
                    }

                    Ok(())
                })
                .unwrap();
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn jit_agrees_with_eval_optimized_small_nursery() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            // `source_file` makes a failure PERSIST to
            // proptest_jit_vs_eval.proptest-regressions — without it,
            // `TestRunner::new` (unlike the proptest! macro) cannot write
            // one at all, which is how jit_small_nursery_agrees's two
            // 2026-08-23 in-shard failures left no repro behind
            // (scratchpad review-nursery-flake.md).
            let mut runner = TestRunner::new(Config {
                cases: 50,
                source_file: Some(file!()),
                ..Config::default()
            });
            runner
                .run(&arb_core_expr(), |mut expr| {
                    optimize(&mut expr).unwrap();
                    check_jit_vs_eval(expr, 4 * 1024)
                })
                .unwrap();
        })
        .unwrap()
        .join()
        .unwrap();
}
