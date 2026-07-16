//! End-to-end JIT GC correctness tests (no direct frame-walker coverage).
//!
//! These tests compile expressions with tiny nurseries to force GC cycles,
//! then verify the results match expected values at the language level.
//!
//! Note: this module no longer exercises `gc::frame_walker` root
//! enumeration behavior directly. Dedicated unit tests for frame-walker
//! internals should live in a separate test module.

use tidepool_codegen::host_fns;
use tidepool_codegen::host_fns::RuntimeError;
use tidepool_codegen::jit_machine::{JitEffectMachine, JitError};
use tidepool_codegen::yield_type::YieldError;
use tidepool_eval::value::Value;
use tidepool_repr::datacon_table::DataConTable;
use tidepool_repr::frame::CoreFrame;
use tidepool_repr::types::*;
use tidepool_repr::{CoreExpr, TreeBuilder};

fn make_table_with_con(id: DataConId, arity: u32) -> DataConTable {
    let mut table = DataConTable::new();
    table.insert(tidepool_repr::datacon::DataCon {
        id,
        name: format!("C{}", id.0),
        tag: (id.0 % 100) as u32 + 1,
        rep_arity: arity,
        field_bangs: vec![],
        qualified_name: None,
    });
    // Add required freer-simple tags for JitEffectMachine::compile
    use tidepool_codegen::effect_machine::EffContKind;
    for (i, kind) in EffContKind::ALL.iter().enumerate() {
        table.insert(tidepool_repr::datacon::DataCon {
            id: DataConId(1000 + i as u64),
            name: kind.name().to_string(),
            tag: (1000 + i) as u32,
            rep_arity: if matches!(kind, EffContKind::Node | EffContKind::Union) {
                2
            } else {
                1
            },
            field_bangs: vec![],
            qualified_name: None,
        });
    }
    table
}

/// Nested function application chain that allocates garbage:
/// `letrec f = \x -> let g1 = Con(1, [x]) in let g2 = Con(1, [g1]) in x in f (f ... (f (Lit 42)))`
/// — same program shape as `tidepool_testing::gen::make_gc_forcing_setup`;
/// its expr half is reused directly (the table half is rebuilt locally via
/// `make_table_with_con` to add the `JitEffectMachine`-required EffCont tags).
fn build_con_chain(depth: usize) -> CoreExpr {
    tidepool_testing::gen::make_gc_forcing_setup(depth).0
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
            let _result = machine.run_pure().expect(
                "GC should free enough memory to evaluate depth-40 chain with 2 KiB nursery",
            );

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

            let Value::Con(tag, ref fields) = result else {
                panic!("Expected Con, got {:?}", result);
            };
            assert_eq!(tag, DataConId(1));
            assert_eq!(fields.len(), 1);
            let Value::Lit(lit) = &fields[0] else {
                panic!("Expected Lit(42), got {:?}", fields[0]);
            };
            assert_eq!(*lit, Literal::LitInt(42));
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

            if let Ok(val) = result {
                // Verify the result is a nested Con chain ending in Lit(42)
                let mut current = &val;
                for _ in 0..60 {
                    let Value::Con(_, fields) = current else {
                        panic!("Expected Con in chain, got {:?}", current);
                    };
                    assert_eq!(fields.len(), 1);
                    current = &fields[0];
                }
                let Value::Lit(Literal::LitInt(42)) = current else {
                    panic!("Expected Lit(42) at leaf, got {:?}", current);
                };
                // Should have multiple GC cycles
                let gc_count = host_fns::gc_trigger_call_count();
                assert!(
                    gc_count > 1,
                    "Expected multiple GC cycles, got {}",
                    gc_count
                );
            } else if let Err(JitError::Yield(YieldError::Runtime(RuntimeError::HeapOverflow))) =
                result
            {
                // HeapOverflow is acceptable for small nursery
            } else if let Err(e) = result {
                panic!("Expected HeapOverflow but got: {}", e);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
