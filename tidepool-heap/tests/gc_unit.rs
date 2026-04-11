use std::collections::HashSet;
use tidepool_heap::gc::raw::for_each_pointer_field;
use tidepool_heap::layout::*;

#[repr(align(8))]
struct AlignedBuf<const N: usize>([u8; N]);

#[test]
fn test_for_each_pointer_field_con_zero_fields() {
    let mut buf_data = AlignedBuf::<1024>([0u8; 1024]);
    let ptr = buf_data.0.as_mut_ptr();
    unsafe {
        let size = CON_FIELDS_OFFSET;
        write_header(ptr, TAG_CON, size as u16);
        *(ptr.add(CON_TAG_OFFSET) as *mut u64) = 42;
        *(ptr.add(CON_NUM_FIELDS_OFFSET) as *mut u16) = 0;

        let mut count = 0;
        for_each_pointer_field(ptr, |_| {
            count += 1;
        });
        assert_eq!(count, 0, "Con with 0 fields should have 0 pointer fields");
    }
}

#[test]
fn test_for_each_pointer_field_closure_zero_captures() {
    let mut buf_data = AlignedBuf::<1024>([0u8; 1024]);
    let ptr = buf_data.0.as_mut_ptr();
    unsafe {
        let size = CLOSURE_CAPTURED_OFFSET;
        write_header(ptr, TAG_CLOSURE, size as u16);
        *(ptr.add(CLOSURE_CODE_PTR_OFFSET) as *mut usize) = 0x12345678;
        *(ptr.add(CLOSURE_NUM_CAPTURED_OFFSET) as *mut u16) = 0;

        let mut count = 0;
        for_each_pointer_field(ptr, |_| {
            count += 1;
        });
        assert_eq!(
            count, 0,
            "Closure with 0 captures should have 0 pointer fields"
        );
    }
}

#[test]
fn test_layout_constant_sanity() {
    let tags = vec![TAG_CLOSURE, TAG_THUNK, TAG_CON, TAG_LIT, TAG_FORWARDED];
    let mut set = HashSet::new();
    for tag in tags {
        assert!(set.insert(tag), "Duplicate tag found: {}", tag);
    }
}

#[test]
fn test_offset_calculations() {
    const _: () = assert!(CON_FIELDS_OFFSET > CON_NUM_FIELDS_OFFSET);
    const _: () = assert!(CON_NUM_FIELDS_OFFSET > CON_TAG_OFFSET);
    const _: () = assert!(CLOSURE_CAPTURED_OFFSET > CLOSURE_NUM_CAPTURED_OFFSET);
    const _: () = assert!(CLOSURE_NUM_CAPTURED_OFFSET > CLOSURE_CODE_PTR_OFFSET);
}

#[test]
fn test_thunk_state_machine() {
    let mut buf_data = AlignedBuf::<THUNK_MIN_SIZE>([0u8; THUNK_MIN_SIZE]);
    let ptr = buf_data.0.as_mut_ptr();
    unsafe {
        write_header(ptr, TAG_THUNK, THUNK_MIN_SIZE as u16);

        // 1. Set Unevaluated
        *(ptr.add(THUNK_STATE_OFFSET)) = THUNK_UNEVALUATED;
        assert_eq!(*(ptr.add(THUNK_STATE_OFFSET)), THUNK_UNEVALUATED);

        // 2. Transition to BlackHole (during evaluation)
        *(ptr.add(THUNK_STATE_OFFSET)) = THUNK_BLACKHOLE;
        assert_eq!(*(ptr.add(THUNK_STATE_OFFSET)), THUNK_BLACKHOLE);

        // 3. Transition to Evaluated
        *(ptr.add(THUNK_STATE_OFFSET)) = THUNK_EVALUATED;
        assert_eq!(*(ptr.add(THUNK_STATE_OFFSET)), THUNK_EVALUATED);
    }
}

#[test]
fn test_gc_thunkref_tracing() {
    use tidepool_eval::env::Env;
    use tidepool_eval::value::Value;
    use tidepool_eval::{Heap, ThunkState};
    use tidepool_heap::ArenaHeap;
    use tidepool_repr::{CoreFrame, Literal, RecursiveTree, VarId};

    let mut heap = ArenaHeap::new();
    let env = Env::new();
    let expr = RecursiveTree {
        nodes: vec![CoreFrame::Var(VarId(0))],
    };

    // 1. Allocate thunk B, evaluate it to Value::Lit(LitInt(99))
    let id_b = heap.alloc(env.clone(), expr.clone());
    heap.write(id_b, ThunkState::Evaluated(Value::Lit(Literal::LitInt(99))));

    // 2. Allocate thunk A, evaluate it to Value::ThunkRef(B) — A's value points to B
    let id_a = heap.alloc(env.clone(), expr.clone());
    heap.write(id_a, ThunkState::Evaluated(Value::ThunkRef(id_b)));

    // 3. Keep only thunk A as a GC root (do NOT keep B as a direct root)
    let table = heap.collect_garbage(&[id_a]);

    // 4. After GC, assert that B is still alive (can be read) and contains Evaluated(LitInt(99))
    let new_id_a = table.lookup(id_a).expect("Thunk A should be alive");

    // Check what id_a points to now
    let ThunkState::Evaluated(Value::ThunkRef(new_id_b)) = heap.read(new_id_a) else {
        panic!("Expected Thunk A to be Evaluated(ThunkRef(_))");
    };
    let new_id_b = *new_id_b;

    // Assert id_b survived and has correct value
    let ThunkState::Evaluated(Value::Lit(Literal::LitInt(99))) = heap.read(new_id_b) else {
        panic!(
            "Expected Thunk B to be Evaluated(LitInt(99)), got {:?}",
            heap.read(new_id_b)
        );
    };

    // Also verify B is in the forwarding table
    assert!(
        table.lookup(id_b).is_ok(),
        "Thunk B should be in forwarding table"
    );
    assert_eq!(table.lookup(id_b).unwrap(), new_id_b);
}
