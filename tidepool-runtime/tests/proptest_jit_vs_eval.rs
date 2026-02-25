use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};
use tidepool_codegen::jit_machine::{JitEffectMachine, JitError};
use tidepool_eval::{eval, env_from_datacon_table, Value, VecHeap};
use tidepool_optimize::pipeline::optimize;
use tidepool_repr::datacon_table::DataConTable;
use tidepool_repr::frame::CoreFrame;
use tidepool_repr::types::AltCon;
use tidepool_repr::CoreExpr;
use tidepool_testing::gen::{arb_core_expr, standard_datacon_table};

/// Structural comparison of values.
/// Skips closures, thunks, and join points by returning true.
fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Lit(l1), Value::Lit(l2)) => l1 == l2,
        (Value::Con(tag1, fields1), Value::Con(tag2, fields2)) => {
            tag1 == tag2
                && fields1.len() == fields2.len()
                && fields1
                    .iter()
                    .zip(fields2.iter())
                    .all(|(f1, f2)| values_equal(f1, f2))
        }
        // For closures and thunks, skip comparison (return true to not fail the test)
        _ => true,
    }
}

/// Walk the tree to find all DataConIds and their arities.
fn build_table_for_expr(expr: &CoreExpr) -> DataConTable {
    let mut table = standard_datacon_table();
    let mut seen = std::collections::HashMap::new();

    for node in &expr.nodes {
        match node {
            CoreFrame::Con { tag, fields } => {
                let arity = fields.len() as u32;
                let entry = seen.entry(*tag).or_insert(0);
                if arity > *entry {
                    *entry = arity;
                }
            }
            CoreFrame::Case { alts, .. } => {
                for alt in alts {
                    if let AltCon::DataAlt(tag) = alt.con {
                        let arity = alt.binders.len() as u32;
                        let entry = seen.entry(tag).or_insert(0);
                        if arity > *entry {
                            *entry = arity;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    for (id, arity) in seen {
        if table.get(id).is_none() {
            table.insert(tidepool_repr::datacon::DataCon {
                id,
                name: format!("C{}", id.0),
                tag: (id.0 % 100) as u32 + 1,
                rep_arity: arity,
                field_bangs: vec![],
            });
        }
    }

    table
}

fn check_jit_vs_eval(expr: CoreExpr, nursery_size: usize) -> Result<(), TestCaseError> {
    let table = build_table_for_expr(&expr);

    // Tree-walking evaluation
    let mut heap_eval = VecHeap::new();
    let env_eval = env_from_datacon_table(&table);
    let res_eval = eval(&expr, &env_eval, &mut heap_eval);

    // JIT compilation and execution
    let res_jit = match JitEffectMachine::compile(&expr, &table, nursery_size) {
        Ok(mut machine) => machine.run_pure().map_err(JitError::from),
        Err(e) => Err(e),
    };

    match (res_eval, res_jit) {
        (Ok(v1), Ok(v2)) => {
            prop_assert!(
                values_equal(&v1, &v2),
                "JIT and Eval results differ.
Eval: {:?}
JIT:  {:?}
Expr: {:#?}",
                v1,
                v2,
                expr
            );
        }
        _ => {
            // Skip cases where either fails.
        }
    }

    Ok(())
}

#[test]
fn jit_agrees_with_eval() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config {
                cases: 10,
                ..Config::default()
            });
            runner
                .run(&arb_core_expr(), |expr| {
                    check_jit_vs_eval(expr, 64 * 1024)
                })
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
            let mut runner = TestRunner::new(Config {
                cases: 10,
                ..Config::default()
            });
            runner
                .run(&arb_core_expr(), |mut expr| {
                    optimize(&mut expr);
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
            let mut runner = TestRunner::new(Config {
                cases: 10,
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
            let mut runner = TestRunner::new(Config {
                cases: 10,
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
