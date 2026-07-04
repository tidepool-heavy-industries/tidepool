use tidepool_codegen::heap_bridge::{heap_to_value, BridgeError};
use tidepool_codegen::host_fns;
use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_codegen::layout::{LIT_TAG_ARRAY, LIT_TAG_SMALLARRAY};
use tidepool_eval::value::Value;
use tidepool_heap::layout;
use tidepool_repr::*;

#[repr(align(8))]
struct AlignedBuf<const N: usize>([u8; N]);

#[test]
fn test_heap_to_value_lit_int() {
    let mut buf_data = AlignedBuf::<{ layout::LIT_SIZE }>([0u8; layout::LIT_SIZE]);
    let ptr = buf_data.0.as_mut_ptr();
    unsafe {
        layout::write_header(ptr, layout::TAG_LIT, layout::LIT_SIZE as u16);
        *(ptr.add(layout::LIT_TAG_OFFSET)) = layout::LitTag::Int as u8;
        *(ptr.add(layout::LIT_VALUE_OFFSET) as *mut i64) = 42;

        let res = heap_to_value(ptr).expect("heap_to_value failed");
        let Value::Lit(Literal::LitInt(n)) = res else {
            panic!("Expected LitInt, got {:?}", res);
        };
        assert_eq!(n, 42);
    }
}

#[test]
fn test_heap_to_value_con_pair() {
    // A pair: Con(DataConId(1), [LitInt(10), LitInt(20)])
    let mut buf_data = AlignedBuf::<1024>([0u8; 1024]);
    let start = buf_data.0.as_mut_ptr();
    unsafe {
        let lit1 = start;
        layout::write_header(lit1, layout::TAG_LIT, layout::LIT_SIZE as u16);
        *(lit1.add(layout::LIT_TAG_OFFSET)) = layout::LitTag::Int as u8;
        *(lit1.add(layout::LIT_VALUE_OFFSET) as *mut i64) = 10;

        let lit2 = start.add(layout::LIT_SIZE);
        layout::write_header(lit2, layout::TAG_LIT, layout::LIT_SIZE as u16);
        *(lit2.add(layout::LIT_TAG_OFFSET)) = layout::LitTag::Int as u8;
        *(lit2.add(layout::LIT_VALUE_OFFSET) as *mut i64) = 20;

        let con = start.add(2 * layout::LIT_SIZE);
        let num_fields = 2;
        let con_size = layout::CON_FIELDS_OFFSET + num_fields * 8;
        layout::write_header(con, layout::TAG_CON, con_size as u16);
        *(con.add(layout::CON_TAG_OFFSET) as *mut u64) = 1;
        *(con.add(layout::CON_NUM_FIELDS_OFFSET) as *mut u16) = num_fields as u16;
        *(con.add(layout::CON_FIELDS_OFFSET) as *mut *const u8) = lit1;
        *(con.add(layout::CON_FIELDS_OFFSET + 8) as *mut *const u8) = lit2;

        let res = heap_to_value(con).expect("heap_to_value failed");
        let Value::Con(DataConId(1), ref fields) = res else {
            panic!("Expected Con, got {:?}", res);
        };
        assert_eq!(fields.len(), 2);
        match (&fields[0], &fields[1]) {
            (Value::Lit(Literal::LitInt(10)), Value::Lit(Literal::LitInt(20))) => (),
            _ => panic!("Expected [LitInt(10), LitInt(20)], got {:?}", fields),
        }
    }
}

#[test]
fn test_heap_to_value_deeply_nested_cons() {
    // Chain of 100 nested Cons: Con(0, [Con(0, [ ... LitInt(0) ... ])])
    let mut buf_data = AlignedBuf::<{ 1024 * 64 }>([0u8; 1024 * 64]);
    let start = buf_data.0.as_mut_ptr();
    unsafe {
        let mut current = start;

        // Leaf LitInt(0)
        layout::write_header(current, layout::TAG_LIT, layout::LIT_SIZE as u16);
        *(current.add(layout::LIT_TAG_OFFSET)) = layout::LitTag::Int as u8;
        *(current.add(layout::LIT_VALUE_OFFSET) as *mut i64) = 0;

        let mut last_ptr = current;
        current = current.add(layout::LIT_SIZE);

        for _ in 0..100 {
            let num_fields = 1;
            let con_size = layout::CON_FIELDS_OFFSET + num_fields * 8;
            layout::write_header(current, layout::TAG_CON, con_size as u16);
            *(current.add(layout::CON_TAG_OFFSET) as *mut u64) = 0;
            *(current.add(layout::CON_NUM_FIELDS_OFFSET) as *mut u16) = num_fields as u16;
            *(current.add(layout::CON_FIELDS_OFFSET) as *mut *const u8) = last_ptr;

            last_ptr = current;
            current = current.add(con_size);
        }

        let res = heap_to_value(last_ptr).expect("heap_to_value failed on deep structure");

        // Verify depth
        let mut depth = 0;
        let mut v = res;
        while let Value::Con(_, ref fields) = v {
            depth += 1;
            let inner = fields[0].clone();
            v = inner;
        }
        assert_eq!(depth, 100);
        let Value::Lit(Literal::LitInt(0)) = v else {
            panic!("Expected terminal LitInt(0), got {:?}", v);
        };
    }
}

#[test]
fn test_heap_to_value_lit_smallarray_null() {
    let mut buf_data = AlignedBuf::<{ layout::LIT_SIZE }>([0u8; layout::LIT_SIZE]);
    let ptr = buf_data.0.as_mut_ptr();
    unsafe {
        layout::write_header(ptr, layout::TAG_LIT, layout::LIT_SIZE as u16);
        *(ptr.add(layout::LIT_TAG_OFFSET)) = LIT_TAG_SMALLARRAY as u8;
        // Null pointer for the array data
        *(ptr.add(layout::LIT_VALUE_OFFSET) as *mut *const u8) = std::ptr::null();

        let res = heap_to_value(ptr);
        assert!(
            matches!(res, Err(BridgeError::NullPointer)),
            "Expected NullPointer error, got {:?}",
            res
        );
    }
}

#[test]
fn test_heap_to_value_lit_array_null() {
    let mut buf_data = AlignedBuf::<{ layout::LIT_SIZE }>([0u8; layout::LIT_SIZE]);
    let ptr = buf_data.0.as_mut_ptr();
    unsafe {
        layout::write_header(ptr, layout::TAG_LIT, layout::LIT_SIZE as u16);
        *(ptr.add(layout::LIT_TAG_OFFSET)) = LIT_TAG_ARRAY as u8;
        // Null pointer for the array data
        *(ptr.add(layout::LIT_VALUE_OFFSET) as *mut *const u8) = std::ptr::null();

        let res = heap_to_value(ptr);
        assert!(
            matches!(res, Err(BridgeError::NullPointer)),
            "Expected NullPointer error, got {:?}",
            res
        );
    }
}

/// Build a Con-chain expr + DataConTable that forces >=1 real GC under a
/// small nursery. Copied from `jit_machine.rs`'s `make_gc_forcing_setup`
/// (each test binary under `tests/` is a separate crate, so it can't be
/// shared directly) — an App-chain calling a function that allocates two
/// Cons per call, `depth` times.
fn build_gc_forcing_program(depth: usize) -> (CoreExpr, DataConTable) {
    let mut bld = TreeBuilder::new();
    let var_x = bld.push(CoreFrame::Var(VarId(0)));
    let g1_rhs = bld.push(CoreFrame::Con {
        tag: DataConId(1),
        fields: vec![var_x],
    });
    let var_g1 = bld.push(CoreFrame::Var(VarId(1)));
    let g2_rhs = bld.push(CoreFrame::Con {
        tag: DataConId(1),
        fields: vec![var_g1],
    });
    let final_con = bld.push(CoreFrame::Con {
        tag: DataConId(1),
        fields: vec![var_x],
    });
    let let_g2 = bld.push(CoreFrame::LetNonRec {
        binder: VarId(2),
        rhs: g2_rhs,
        body: final_con,
    });
    let let_g1 = bld.push(CoreFrame::LetNonRec {
        binder: VarId(1),
        rhs: g1_rhs,
        body: let_g2,
    });
    let lam_x = bld.push(CoreFrame::Lam {
        binder: VarId(0),
        body: let_g1,
    });
    let mut current = bld.push(CoreFrame::Lit(Literal::LitInt(42)));
    for _ in 0..depth {
        let f_var = bld.push(CoreFrame::Var(VarId(99)));
        current = bld.push(CoreFrame::App {
            fun: f_var,
            arg: current,
        });
    }
    bld.push(CoreFrame::LetRec {
        bindings: vec![(VarId(99), lam_x)],
        body: current,
    });
    let expr = bld.build();

    let mut table = DataConTable::new();
    table.insert(DataCon {
        id: DataConId(1),
        name: "C1".to_string(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: None,
    });
    (expr, table)
}

/// Regression test for the null-vmctx temporal-safety invariant documented on
/// `heap_to_value`: a `Value` bridged with NO machine installed (the
/// null-vmctx path — this file's other tests all exercise it, since none of
/// them ever construct a `JitEffectMachine`) retains no pointer into any JIT
/// heap, so it must stay byte-identical across an unrelated, LATER real JIT
/// run that triggers actual GCs.
///
/// This is what makes the null-vmctx root-registration no-op safe rather than
/// a silently-skipped safety measure: there is nothing here for a later
/// collection to relocate or dangle.
#[test]
fn null_vmctx_bridge_survives_later_gc() {
    // 1. Build a small heap object entirely OUTSIDE any JIT machine (an
    //    ordinary Rust buffer on this test's stack) and bridge it via the
    //    null-vmctx path.
    let mut buf_data = AlignedBuf::<1024>([0u8; 1024]);
    let start = buf_data.0.as_mut_ptr();
    let bridged = unsafe {
        let lit_int = start;
        layout::write_header(lit_int, layout::TAG_LIT, layout::LIT_SIZE as u16);
        *(lit_int.add(layout::LIT_TAG_OFFSET)) = layout::LitTag::Int as u8;
        *(lit_int.add(layout::LIT_VALUE_OFFSET) as *mut i64) = 123;

        let con = start.add(layout::LIT_SIZE);
        let num_fields = 1;
        let con_size = layout::CON_FIELDS_OFFSET + num_fields * 8;
        layout::write_header(con, layout::TAG_CON, con_size as u16);
        *(con.add(layout::CON_TAG_OFFSET) as *mut u64) = 7;
        *(con.add(layout::CON_NUM_FIELDS_OFFSET) as *mut u16) = num_fields as u16;
        *(con.add(layout::CON_FIELDS_OFFSET) as *mut *const u8) = lit_int;

        // heap_to_value(ptr) is the null-vmctx path (see its definition).
        heap_to_value(con).expect("heap_to_value failed")
    };

    fn expect_shape(v: &Value) -> bool {
        matches!(v, Value::Con(DataConId(7), fields)
            if fields.len() == 1 && matches!(fields[0], Value::Lit(Literal::LitInt(123))))
    }
    assert!(
        expect_shape(&bridged),
        "unexpected bridged shape before GC: {:?}",
        bridged
    );

    // Overwrite the SOURCE buffer with garbage. The bridge output must be a
    // complete OWNED deep copy retaining no pointer back into it — so this
    // corruption of the source must leave `bridged` untouched. (Were the
    // output to reference the source, the assert below would observe it; this
    // is what makes the later-GC check meaningful rather than tautological.)
    unsafe {
        std::ptr::write_bytes(start, 0xEE, buf_data.0.len());
    }
    assert!(
        expect_shape(&bridged),
        "bridged value references the mutated source buffer (not a deep copy): {:?}",
        bridged
    );

    // 2. Drive a REAL JIT run, over a tiny nursery, on a totally SEPARATE
    //    machine/heap — one that forces at least one real GC.
    let (expr, table) = build_gc_forcing_program(40);
    let mut machine = JitEffectMachine::compile(&expr, &table, 2048).expect("compile");
    host_fns::reset_test_counters();
    let _ = machine.run_pure().expect("run_pure should succeed");
    assert!(
        host_fns::gc_trigger_call_count() > 0,
        "the forcing program must have triggered at least one real GC"
    );

    // 3. The earlier null-vmctx bridge output must be untouched: it is an
    //    owned deep copy that never pointed into any JIT heap, so the later
    //    machine's collection(s) cannot have relocated or corrupted it.
    assert!(
        expect_shape(&bridged),
        "bridged value corrupted after an unrelated later GC: {:?}",
        bridged
    );
}
