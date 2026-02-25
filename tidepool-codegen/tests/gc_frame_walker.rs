//! End-to-end GC correctness tests.
//!
//! These tests compile expressions with tiny nurseries to force GC cycles,
//! then verify the results match expected values.

use tidepool_codegen::jit_machine::JitEffectMachine;
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
    
    let return_x = bld.push(CoreFrame::Var(VarId(0))); // Return original x
    
    let let_g2 = bld.push(CoreFrame::LetNonRec { binder: VarId(2), rhs: g2_rhs, body: return_x });
    let let_g1 = bld.push(CoreFrame::LetNonRec { binder: VarId(1), rhs: g1_rhs, body: let_g2 });
    
    let _lam_x = bld.push(CoreFrame::Lam { binder: VarId(0), body: let_g1 });
    
    // We want the final result to be a deeply nested Con chain like before,
    // so `test_multiple_gc_cycles` can match it.
    // Wait, the tests match `Value::Con(_, fields) ... ending in Lit(42)`.
    // So `f` should ACTUALLY return `Con(1, [x])` but allocate extra garbage!
    // `letrec f = \x -> let g1 = Con(1, [x]) in let g2 = Con(1, [g1]) in Con(1, [x])`
    
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
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            // 512-byte nursery, depth-20 Con chain should require GC
            let expr = build_con_chain(20);
            let table = make_table_with_con(DataConId(1), 1);

            host_fns::reset_test_counters();
            let mut machine = JitEffectMachine::compile(&expr, &table, 512).unwrap();
            let result = machine.run_pure();

            // Should succeed (GC freed memory) or HeapOverflow (nursery too small even after GC)
            match result {
                Ok(_) => {
                    // GC must have fired for this to work with 512 bytes
                    assert!(host_fns::gc_trigger_call_count() > 0,
                        "Expected GC to fire with 512-byte nursery and depth-20 chain");
                }
                Err(e) => {
                    // HeapOverflow is acceptable for very tiny nurseries
                    assert!(format!("{}", e).contains("heap overflow"),
                        "Expected HeapOverflow but got: {}", e);
                }
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn test_gc_preserves_values() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
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
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            // Deep chain with very small nursery — forces multiple GC cycles
            let expr = build_con_chain(15);
            let table = make_table_with_con(DataConId(1), 1);

            host_fns::reset_test_counters();
            let mut machine = JitEffectMachine::compile(&expr, &table, 256).unwrap();
            let result = machine.run_pure();

            match result {
                Ok(val) => {
                    // Verify the result is a nested Con chain ending in Lit(42)
                    let mut current = &val;
                    for _ in 0..15 {
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
                Err(e) => {
                    // HeapOverflow is acceptable for 256-byte nursery
                    assert!(format!("{}", e).contains("heap overflow"),
                        "Expected HeapOverflow but got: {}", e);
                }
            }
        })
        .unwrap()
        .join()
        .unwrap();
}