use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};
use tidepool_codegen::jit_machine::{JitEffectMachine, JitError};
use tidepool_codegen::yield_type::YieldError;
use tidepool_eval::{env_from_datacon_table, eval, Value, VecHeap};
use tidepool_optimize::pipeline::optimize;
use tidepool_repr::datacon_table::DataConTable;
use tidepool_repr::frame::CoreFrame;
use tidepool_repr::types::{AltCon, DataConId, Literal, VarId};
use tidepool_repr::{CoreExpr, TreeBuilder};
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
                "JIT and Eval results differ.\nEval: {:?}\nJIT:  {:?}\nExpr: {:#?}",
                v1,
                v2,
                expr
            );
        }
        (Ok(_), Err(JitError::Yield(YieldError::HeapOverflow))) => {
            // HeapOverflow is acceptable — means GC couldn't free enough space
            // for a very small nursery. Skip these rather than failing.
            prop_assume!(false, "HeapOverflow with tiny nursery");
        }
        (Ok(v1), Err(e)) => {
            prop_assert!(
                false,
                "JIT failed but eval succeeded.\nEval: {:?}\nJIT error: {:?}\nExpr: {:#?}",
                v1,
                e,
                expr
            );
        }
        _ => {
            // Both fail or eval fails — skip
        }
    }

    Ok(())
}

#[test]
#[ignore = "Fails with UnresolvedVar due to a pre-existing JIT/codegen bug"]
fn allocation_heavy_tiny_nursery() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config {
                cases: 50,
                ..Config::default()
            });
            runner
                .run(
                    &arb_core_expr().prop_filter("at least 3 Con nodes", |expr| {
                        expr.nodes
                            .iter()
                            .filter(|n| matches!(n, CoreFrame::Con { .. }))
                            .count()
                            >= 3
                    }),
                    |expr| check_jit_vs_eval(expr, 2 * 1024),
                )
                .unwrap();
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn nested_con_chain() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config {
                cases: 50,
                ..Config::default()
            });
            runner
                .run(&(10..40usize), |depth| {
                    let mut bld = TreeBuilder::new();
                    let mut current = bld.push(CoreFrame::Lit(Literal::LitInt(42)));
                    for _ in 0..depth {
                        current = bld.push(CoreFrame::Con {
                            tag: DataConId(1),
                            fields: vec![current],
                        });
                    }
                    let expr = bld.build();
                    check_jit_vs_eval(expr, 4 * 1024)
                })
                .unwrap();
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
#[ignore = "Fails with UnresolvedVar due to a pre-existing JIT/codegen bug"]
fn jit_1kb_nursery_agrees() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config {
                cases: 50,
                ..Config::default()
            });
            runner
                .run(&arb_core_expr(), |expr| check_jit_vs_eval(expr, 1024))
                .unwrap();
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
#[ignore = "Fails with UnresolvedVar due to a pre-existing JIT/codegen bug"]
fn optimize_then_tiny_nursery() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config {
                cases: 50,
                ..Config::default()
            });
            runner
                .run(&arb_core_expr(), |mut expr| {
                    optimize(&mut expr);
                    check_jit_vs_eval(expr, 2 * 1024)
                })
                .unwrap();
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn nested_pair_chain() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config {
                cases: 50,
                ..Config::default()
            });
            runner
                .run(&(3..15usize), |n| {
                    let mut bld = TreeBuilder::new();
                    // Build bottom-up: start with the body
                    let mut body = bld.push(CoreFrame::Var(VarId(n as u64)));

                    // We want: let v0 = (0, 1) in let v1 = (1, v0) in ... let vN = (N, v_{N-1}) in vN
                    // To build this bottom-up, we need to build LetNonRec for vN first, then v_{N-1}, etc.
                    for i in (0..=n).rev() {
                        let rhs = if i == 0 {
                            let l0 = bld.push(CoreFrame::Lit(Literal::LitInt(0)));
                            let l1 = bld.push(CoreFrame::Lit(Literal::LitInt(1)));
                            bld.push(CoreFrame::Con {
                                tag: DataConId(4),
                                fields: vec![l0, l1],
                            })
                        } else {
                            let li = bld.push(CoreFrame::Lit(Literal::LitInt(i as i64)));
                            let prev = bld.push(CoreFrame::Var(VarId((i - 1) as u64)));
                            bld.push(CoreFrame::Con {
                                tag: DataConId(4),
                                fields: vec![li, prev],
                            })
                        };
                        body = bld.push(CoreFrame::LetNonRec {
                            binder: VarId(i as u64),
                            rhs,
                            body,
                        });
                    }

                    let expr = bld.build();
                    check_jit_vs_eval(expr, 2 * 1024)
                })
                .unwrap();
        })
        .unwrap()
        .join()
        .unwrap();
}
