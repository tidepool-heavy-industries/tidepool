use std::collections::HashSet;
use tidepool_heap::gc::raw::for_each_pointer_field;
use tidepool_heap::layout::*;

const _: () = assert!(CON_FIELDS_OFFSET > CON_NUM_FIELDS_OFFSET);
const _: () = assert!(CON_NUM_FIELDS_OFFSET > CON_TAG_OFFSET);
const _: () = assert!(CLOSURE_CAPTURED_OFFSET > CLOSURE_NUM_CAPTURED_OFFSET);
const _: () = assert!(CLOSURE_NUM_CAPTURED_OFFSET > CLOSURE_CODE_PTR_OFFSET);

#[repr(align(8))]
struct AlignedBuf<const N: usize>([u8; N]);

#[test]
fn test_for_each_pointer_field_con_zero_fields() {
    let mut buf_data = AlignedBuf::<1024>([0u8; 1024]);
    let ptr = buf_data.0.as_mut_ptr();
    unsafe {
        let size = CON_FIELDS_OFFSET;
        write_header(ptr, TAG_CON, size as u32);
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
        write_header(ptr, TAG_CLOSURE, size as u32);
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

