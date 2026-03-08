//! Option 4: Optimization preserves evaluation semantics.
//!
//! For every generated expression, `eval(expr) == eval(optimize(expr))`.
//! Catches optimizer soundness bugs where a transformation changes behavior.

use proptest::test_runner::{Config, TestRunner};
use tidepool_eval::{env::Env, eval::eval, heap::VecHeap};
use tidepool_optimize::optimize;
use tidepool_testing::compare;
use tidepool_testing::gen::arb_ground_expr;

#[test]
fn optimization_preserves_semantics() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config {
                cases: 200,
                ..Config::default()
            });
            runner
                .run(&arb_ground_expr(), |expr| {
                    // Evaluate original
                    let mut heap1 = VecHeap::new();
                    let before = eval(&expr, &Env::new(), &mut heap1);

                    // Optimize
                    let mut optimized = expr.clone();
                    let _ = optimize(&mut optimized); // ignore stats

                    // Evaluate optimized
                    let mut heap2 = VecHeap::new();
                    let after = eval(&optimized, &Env::new(), &mut heap2);

                    match (before, after) {
                        (Ok(v1), Ok(v2)) => {
                            let f1 = tidepool_eval::eval::deep_force(v1, &mut heap1);
                            let f2 = tidepool_eval::eval::deep_force(v2, &mut heap2);
                            match (f1, f2) {
                                (Ok(fv1), Ok(fv2)) => {
                                    compare::assert_values_eq(&fv1, &fv2);
                                }
                                (Err(_), Err(_)) => {} // both error during deep_force
                                (Ok(v), Err(e)) => {
                                    panic!("optimize broke deep_force: before Ok({}) after Err({:?})", v, e)
                                }
                                (Err(_), Ok(_)) => {} // optimizer fixed an error — acceptable
                            }
                        }
                        (Err(_), Err(_)) => {} // both error — acceptable
                        (Ok(_), Err(e)) => {
                            panic!("optimize broke eval: before Ok, after Err({:?})", e)
                        }
                        (Err(_), Ok(_)) => {} // optimizer fixed an error — acceptable
                    }
                    Ok(())
                })
                .unwrap();
        })
        .unwrap();
    handle.join().unwrap();
}
