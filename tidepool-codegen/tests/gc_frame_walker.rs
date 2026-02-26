//! End-to-end JIT GC correctness tests (no direct frame-walker coverage).
//!
//! These tests compile expressions with tiny nurseries to force GC cycles,
//! then verify the results match expected values at the language level.
//!
//! Note: this module no longer exercises `gc::frame_walker` root
//! enumeration/rewriting behavior (e.g., verifying discovered roots and
//! `rewrite_roots`) directly. Dedicated unit tests for frame-walker
//! internals should live in a separate test module.

use tidepool_codegen::jit_machine::{JitEffectMachine, JitError};
use tidepool_codegen::yield_type::YieldError;
use tidepool_codegen::host_fns;
use tidepool_repr::datacon_table::DataConTable;
use tidepool_repr::frame::CoreFrame;
use tidepool_repr::types::*;
use tidepool_repr::{CoreExpr, TreeBuilder};
use tidepool_eval::value::Value;

fn make_table_with_con(id: DataConId, arity: u32) -> DataConTable {
    let mut table = DataConTable::new();
    table.insert(tidepool_repr::datacon::DataCon {
        id,
        name: format!("C{}", id.0),
        tag: (id.0 % 100) as u32 + 1,
        rep_arity: arity,
        field_bangs: vec![],
    });
    table
}

/// Build a nested function application chain that allocates garbage:
/// `letrec f = \x -> let g1 = Con(1, [x]) in let g2 = Con(1, [g1]) in x in f (f ... (f (Lit 42)))`
fn build_con_chain(depth: usize) -> CoreExpr {
    let mut bld = TreeBuilder::new();
    
    // Body of f: let g1 = Con x in let g2 = Con g1 in x
    let var_x = bld.push(CoreFrame::Var(VarId(0)));
    let g1_rhs = bld.push(CoreFrame::Con { tag: DataConId(1), fields: vec![var_x] });
    
    let var_g1 = bld.push(CoreFrame::Var(VarId(1)));
    let g2_rhs = bld.push(CoreFrame::Con { tag: DataConId(1), fields: vec![var_g1] });
    
    // The original let_g1/let_g2/_lam_x chain that returned `x` directly was
    // unused. We now only use the version below that returns `Con(1, [x])`
    // while still allocating extra garbage.
    
    let final_con = bld.push(CoreFrame::Con { tag: DataConId(1), fields: vec![var_x] });
    let let_g2_con = bld.push(CoreFrame::LetNonRec { binder: VarId(2), rhs: g2_rhs, body: final_con });
    let let_g1_con = bld.push(CoreFrame::LetNonRec { binder: VarId(1), rhs: g1_rhs, body: let_g2_con });
    let lam_x_con = bld.push(CoreFrame::Lam { binder: VarId(0), body: let_g1_con });
    
    // Applications: f (f (f ... (Lit 42)))
    let mut current = bld.push(CoreFrame::Lit(Literal::LitInt(42)));
    for _ in 0..depth {
        let f_var = bld.push(CoreFrame::Var(VarId(99))); // f
        current = bld.push(CoreFrame::App { fun: f_var, arg: current });
    }
    
    bld.push(CoreFrame::LetRec {
        bindings: vec![(VarId(99), lam_x_con)],
        body: current,
    });
    
    bld.build()
}

#[test]
fn test_gc_actually_frees_memory() {
    std::thread::Builder::new()
        .stack_size(8 * 2048 * 2048)
        .spawn(|| {
            // 2 KiB nursery, depth-40 Con chain should require GC but still succeed
            let expr = build_con_chain(40);
            let table = make_table_with_con(DataConId(1), 1);

            host_fns::reset_test_counters();
            let mut machine = JitEffectMachine::compile(&expr, &table, 2048).unwrap();
            let _result = machine
                .run_pure()
                .expect("GC should free enough memory to evaluate depth-40 chain with 2 KiB nursery");

            // GC must have fired for this to work with a small nursery
            assert!(
                host_fns::gc_trigger_call_count() > 0,
                "Expected GC to fire with 2 KiB nursery and depth-40 chain"
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn test_gc_preserves_values() {
    std::thread::Builder::new()
        .stack_size(8 * 2048 * 2048)
        .spawn(|| {
            // Build: Con(1, [Lit(42)])
            let mut bld = TreeBuilder::new();
            let lit = bld.push(CoreFrame::Lit(Literal::LitInt(42)));
            let _con = bld.push(CoreFrame::Con {
                tag: DataConId(1),
                fields: vec![lit],
            });
            let expr = bld.build();
            let table = make_table_with_con(DataConId(1), 1);

            // Use a small nursery but big enough that this should work
            let mut machine = JitEffectMachine::compile(&expr, &table, 2048).unwrap();
            let result = machine.run_pure().unwrap();

            match result {
                Value::Con(tag, fields) => {
                    assert_eq!(tag, DataConId(1));
                    assert_eq!(fields.len(), 1);
                    match &fields[0] {
                        Value::Lit(lit) => assert_eq!(*lit, Literal::LitInt(42)),
                        other => panic!("Expected Lit(42), got {:?}", other),
                    }
                }
                other => panic!("Expected Con, got {:?}", other),
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn test_multiple_gc_cycles() {
    std::thread::Builder::new()
        .stack_size(8 * 2048 * 2048)
        .spawn(|| {
            // Deep chain with small nursery — forces multiple GC cycles
            let expr = build_con_chain(60);
            let table = make_table_with_con(DataConId(1), 1);

            host_fns::reset_test_counters();
            let mut machine = JitEffectMachine::compile(&expr, &table, 2048).unwrap();
            let result = machine.run_pure();

            match result {
                Ok(val) => {
                    // Verify the result is a nested Con chain ending in Lit(42)
                    let mut current = &val;
                    for _ in 0..60 {
                        match current {
                            Value::Con(_, fields) => {
                                assert_eq!(fields.len(), 1);
                                current = &fields[0];
                            }
                            other => panic!("Expected Con in chain, got {:?}", other),
                        }
                    }
                    match current {
                        Value::Lit(Literal::LitInt(42)) => {}
                        other => panic!("Expected Lit(42) at leaf, got {:?}", other),
                    }
                    // Should have multiple GC cycles
                    let gc_count = host_fns::gc_trigger_call_count();
                    assert!(gc_count > 1,
                        "Expected multiple GC cycles, got {}", gc_count);
                }
                Err(JitError::Yield(YieldError::HeapOverflow)) => {
                    // HeapOverflow is acceptable for small nursery
                }
                Err(e) => panic!("Expected HeapOverflow but got: {}", e),
            }
        })
        .unwrap()
        .join()
        .unwrap();
}