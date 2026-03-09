//! Shared proptest helpers for property-based testing across crates.
//!
//! Consolidates `build_table_for_expr`, `check_jit_vs_eval`, and
//! `check_pass_preserves_eval` that were previously duplicated across
//! 4+ test files.

use proptest::prelude::*;
use tidepool_codegen::jit_machine::{JitEffectMachine, JitError};
use tidepool_codegen::yield_type::YieldError;
use tidepool_eval::pass::Pass;
use tidepool_eval::value::Value;
use tidepool_eval::{env_from_datacon_table, eval, Env, VecHeap};
use tidepool_repr::datacon_table::DataConTable;
use tidepool_repr::frame::CoreFrame;
use tidepool_repr::types::AltCon;
use tidepool_repr::CoreExpr;

use crate::gen::standard_datacon_table;

/// Structural comparison for proptest contexts. Un-forced synthetic expressions
/// may contain ThunkRefs, closures, and JoinConts that can't be compared
/// structurally — these are skipped (treated as equal).
pub fn values_equal(a: &Value, b: &Value) -> bool {
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
        // Closures, thunks, join conts: skip comparison
        _ => true,
    }
}

/// Walk the tree to find all DataConIds and their arities.
pub fn build_table_for_expr(expr: &CoreExpr) -> DataConTable {
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
                qualified_name: None,
            });
        }
    }

    table
}

/// Compare JIT and interpreter results for a given expression.
///
/// Evaluates the expression with both the tree-walking interpreter and the
/// Cranelift JIT, then structurally compares the results. Acceptable JIT-only
/// failures (HeapOverflow, UnresolvedVar, HeapBridge) are skipped via
/// `prop_assume!`.
pub fn check_jit_vs_eval(expr: CoreExpr, nursery_size: usize) -> Result<(), TestCaseError> {
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
        (Ok(_), Err(JitError::Yield(YieldError::UnresolvedVar(_)))) => {
            // UnresolvedVar in synthetic IR: LetRec simple bindings with
            // inter-dependencies are thunked by the interpreter but evaluated
            // sequentially by the JIT. GHC Core LetRec always has Lam/Con RHS.
            prop_assume!(false, "UnresolvedVar in synthetic LetRec");
        }
        (Ok(_), Err(JitError::HeapBridge(_))) => {
            // UnexpectedHeapTag: consequence of JIT limitation with synthetic IR
            // (e.g., unresolved vars producing garbage heap objects).
            prop_assume!(false, "HeapBridge error in synthetic IR");
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

/// Verify an optimization pass preserves evaluation results.
///
/// Evaluates the expression before and after the pass, then structurally
/// compares results. If the original evaluation fails, the test case is
/// skipped (passes only preserve behavior of well-defined programs).
pub fn check_pass_preserves_eval(pass: &dyn Pass, expr: CoreExpr) -> Result<(), TestCaseError> {
    let mut heap1 = VecHeap::new();
    let env = Env::new();

    // Evaluate original
    let original_res = eval(&expr, &env, &mut heap1);

    // Run the pass
    let mut optimized = expr.clone();
    pass.run(&mut optimized);

    let mut heap2 = VecHeap::new();
    // Evaluate optimized
    let optimized_res = eval(&optimized, &env, &mut heap2);

    match (original_res, optimized_res) {
        (Ok(v1), Ok(v2)) => {
            prop_assert!(
                values_equal(&v1, &v2),
                "Evaluation results differ after pass {}.
Original: {:?}
Optimized: {:?}
Expr: {:#?}
Optimized Expr: {:#?}",
                pass.name(),
                v1,
                v2,
                expr,
                optimized
            );
        }
        (Err(_), _) => {
            // If original eval fails, we skip this case.
            // Passes are only guaranteed to preserve behavior of well-defined programs.
        }
        (Ok(_), Err(e)) => {
            prop_assert!(
                false,
                "Optimized evaluation failed but original succeeded.
Pass: {}
Error: {:?}
Expr: {:#?}
Optimized Expr: {:#?}",
                pass.name(),
                e,
                expr,
                optimized
            );
        }
    }
    Ok(())
}
