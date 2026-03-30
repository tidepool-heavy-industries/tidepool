use tidepool_codegen::heap_bridge::heap_to_value;
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
        let Value::Con(DataConId(1), fields) = res else {
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
        while let Value::Con(_, fields) = v {
            depth += 1;
            v = fields[0].clone();
        }
        assert_eq!(depth, 100);
        let Value::Lit(Literal::LitInt(0)) = v else {
            panic!("Expected terminal LitInt(0), got {:?}", v);
        };
    }
}
