//! Cheney's semi-space copying GC for raw HeapObjects.

use crate::layout::*;

pub struct CopyResult {
    pub bytes_copied: usize,
}

fn is_in_range(ptr: *const u8, start: *const u8, end: *const u8) -> bool {
    (ptr as usize) >= (start as usize) && (ptr as usize) < (end as usize)
}

unsafe fn evacuate(
    old_ptr: *mut u8,
    to_base: *mut u8,
    free: &mut usize,
) -> *mut u8 {
    let tag = read_tag(old_ptr);
    if tag == TAG_FORWARDED {
        return *(old_ptr.add(8) as *const *mut u8);
    }
    let size = read_size(old_ptr) as usize;
    let aligned = (size + 7) & !7;
    let new_ptr = to_base.add(*free);
    std::ptr::copy_nonoverlapping(old_ptr, new_ptr, aligned);
    *old_ptr = TAG_FORWARDED;
    *(old_ptr.add(8) as *mut *mut u8) = new_ptr;
    *free += aligned;
    new_ptr
}

pub unsafe fn for_each_pointer_field(obj: *mut u8, mut f: impl FnMut(*mut *mut u8)) {
    let tag = read_tag(obj);
    let size = read_size(obj) as usize;
    match tag {
        TAG_CLOSURE => {
            let n = *(obj.add(CLOSURE_NUM_CAPTURED_OFFSET) as *const u16) as usize;
            for i in 0..n {
                f(obj.add(CLOSURE_CAPTURED_OFFSET + i * FIELD_STRIDE) as *mut *mut u8);
            }
        }
        TAG_CON => {
            let n = *(obj.add(CON_NUM_FIELDS_OFFSET) as *const u16) as usize;
            for i in 0..n {
                f(obj.add(CON_FIELDS_OFFSET + i * FIELD_STRIDE) as *mut *mut u8);
            }
        }
        TAG_THUNK => {
            let state = *obj.add(THUNK_STATE_OFFSET);
            match state {
                THUNK_UNEVALUATED => {
                    let n = (size - THUNK_CAPTURED_OFFSET) / FIELD_STRIDE;
                    for i in 0..n {
                        f(obj.add(THUNK_CAPTURED_OFFSET + i * FIELD_STRIDE) as *mut *mut u8);
                    }
                }
                THUNK_EVALUATED => {
                    f(obj.add(THUNK_INDIRECTION_OFFSET) as *mut *mut u8);
                }
                _ => {}
            }
        }
        TAG_LIT | _ => {}
    }
}

pub unsafe fn cheney_copy(
    root_ptrs: &[*mut *mut u8],
    from_start: *const u8,
    from_end: *const u8,
    tospace: &mut [u8],
) -> CopyResult {
    let to_base = tospace.as_mut_ptr();
    let mut free: usize = 0;
    // Evacuate roots
    for &root_slot in root_ptrs {
        let old_ptr = *root_slot;
        if !old_ptr.is_null() && is_in_range(old_ptr as *const u8, from_start, from_end) {
            let new_ptr = evacuate(old_ptr, to_base, &mut free);
            *root_slot = new_ptr;
        }
    }
    // Cheney scan
    let mut scan: usize = 0;
    while scan < free {
        let obj = to_base.add(scan);
        let obj_size = read_size(obj) as usize;
        let aligned = (obj_size + 7) & !7;
        for_each_pointer_field(obj, |field_slot| {
            let field_val = *field_slot;
            if !field_val.is_null() && is_in_range(field_val as *const u8, from_start, from_end) {
                let new_ptr = evacuate(field_val, to_base, &mut free);
                *field_slot = new_ptr;
            }
        });
        scan += aligned;
    }
    CopyResult { bytes_copied: free }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[repr(align(8))]
    struct AlignedBuf([u8; 1024]);

    unsafe fn write_lit(buf: &mut [u8], offset: usize, value: i64) -> usize {
        let ptr = buf.as_mut_ptr().add(offset);
        write_header(ptr, TAG_LIT, LIT_SIZE as u16);
        *ptr.add(LIT_TAG_OFFSET) = LitTag::Int as u8;
        *(ptr.add(LIT_VALUE_OFFSET) as *mut i64) = value;
        offset + LIT_SIZE
    }

    unsafe fn write_con(buf: &mut [u8], offset: usize, con_tag: u64, fields: &[*mut u8]) -> usize {
        let ptr = buf.as_mut_ptr().add(offset);
        let size = (CON_FIELDS_OFFSET + fields.len() * FIELD_STRIDE) as u16;
        let aligned = (size + 7) & !7;
        write_header(ptr, TAG_CON, size);
        *(ptr.add(CON_TAG_OFFSET) as *mut u64) = con_tag;
        *(ptr.add(CON_NUM_FIELDS_OFFSET) as *mut u16) = fields.len() as u16;
        for (i, &f) in fields.iter().enumerate() {
            *(ptr.add(CON_FIELDS_OFFSET + i * FIELD_STRIDE) as *mut *mut u8) = f;
        }
        offset + aligned as usize
    }

    unsafe fn write_closure(buf: &mut [u8], offset: usize, code_ptr: *const u8, captures: &[*mut u8]) -> usize {
        let ptr = buf.as_mut_ptr().add(offset);
        let size = (CLOSURE_CAPTURED_OFFSET + captures.len() * FIELD_STRIDE) as u16;
        let aligned = (size + 7) & !7;
        write_header(ptr, TAG_CLOSURE, size);
        *(ptr.add(CLOSURE_CODE_PTR_OFFSET) as *mut *const u8) = code_ptr;
        *(ptr.add(CLOSURE_NUM_CAPTURED_OFFSET) as *mut u16) = captures.len() as u16;
        for (i, &c) in captures.iter().enumerate() {
            *(ptr.add(CLOSURE_CAPTURED_OFFSET + i * FIELD_STRIDE) as *mut *mut u8) = c;
        }
        offset + aligned as usize
    }

    // 1. test_copy_single_lit
    #[test]
    fn test_copy_single_lit() {
        let mut from_buf = AlignedBuf([0u8; 1024]);
        let mut to_buf = AlignedBuf([0u8; 1024]);
        let from = &mut from_buf.0;
        let to = &mut to_buf.0;
        unsafe {
            let _offset = write_lit(from, 0, 42);
            let mut root = from.as_mut_ptr();
            let roots = [&mut root as *mut *mut u8];
            let res = cheney_copy(&roots, from.as_ptr(), from.as_ptr().add(1024), to);
            assert_eq!(res.bytes_copied, LIT_SIZE);
            assert_eq!(root, to.as_mut_ptr());
            assert_eq!(read_tag(root), TAG_LIT);
            assert_eq!(*(root.add(LIT_VALUE_OFFSET) as *const i64), 42);
        }
    }

    // 2. test_copy_con_with_lit_fields
    #[test]
    fn test_copy_con_with_lit_fields() {
        let mut from_buf = AlignedBuf([0u8; 1024]);
        let mut to_buf = AlignedBuf([0u8; 1024]);
        let from = &mut from_buf.0;
        let to = &mut to_buf.0;
        unsafe {
            let off1 = write_lit(from, 0, 10);
            let off2 = write_lit(from, off1, 20);
            let lit1 = from.as_mut_ptr();
            let lit2 = from.as_mut_ptr().add(off1);
            let _off3 = write_con(from, off2, 99, &[lit1, lit2]);
            let mut root = from.as_mut_ptr().add(off2);
            let roots = [&mut root as *mut *mut u8];
            let _res = cheney_copy(&roots, from.as_ptr(), from.as_ptr().add(1024), to);
            
            assert_eq!(root, to.as_mut_ptr()); // con is copied first
            assert_eq!(read_tag(root), TAG_CON);
            let n_fields = *(root.add(CON_NUM_FIELDS_OFFSET) as *const u16);
            assert_eq!(n_fields, 2);
            
            let f1 = *(root.add(CON_FIELDS_OFFSET) as *const *mut u8);
            let f2 = *(root.add(CON_FIELDS_OFFSET + FIELD_STRIDE) as *const *mut u8);
            assert_eq!(read_tag(f1), TAG_LIT);
            assert_eq!(read_tag(f2), TAG_LIT);
            assert_eq!(*(f1.add(LIT_VALUE_OFFSET) as *const i64), 10);
            assert_eq!(*(f2.add(LIT_VALUE_OFFSET) as *const i64), 20);
        }
    }

    // 3. test_copy_closure_with_captures
    #[test]
    fn test_copy_closure_with_captures() {
        let mut from_buf = AlignedBuf([0u8; 1024]);
        let mut to_buf = AlignedBuf([0u8; 1024]);
        let from = &mut from_buf.0;
        let to = &mut to_buf.0;
        unsafe {
            let off1 = write_lit(from, 0, 100);
            let lit = from.as_mut_ptr();
            let code_ptr = 0x12345678usize as *const u8;
            let _off2 = write_closure(from, off1, code_ptr, &[lit]);
            let mut root = from.as_mut_ptr().add(off1);
            let roots = [&mut root as *mut *mut u8];
            let _res = cheney_copy(&roots, from.as_ptr(), from.as_ptr().add(1024), to);
            
            assert_eq!(root, to.as_mut_ptr());
            assert_eq!(read_tag(root), TAG_CLOSURE);
            assert_eq!(*(root.add(CLOSURE_CODE_PTR_OFFSET) as *const *const u8), code_ptr);
            
            let cap = *(root.add(CLOSURE_CAPTURED_OFFSET) as *const *mut u8);
            assert_eq!(read_tag(cap), TAG_LIT);
            assert_eq!(*(cap.add(LIT_VALUE_OFFSET) as *const i64), 100);
        }
    }

    // 4. test_transitive_chain
    #[test]
    fn test_transitive_chain() {
        let mut from_buf = AlignedBuf([0u8; 1024]);
        let mut to_buf = AlignedBuf([0u8; 1024]);
        let from = &mut from_buf.0;
        let to = &mut to_buf.0;
        unsafe {
            let off1 = write_lit(from, 0, 7);
            let lit = from.as_mut_ptr();
            let off2 = write_con(from, off1, 1, &[lit]);
            let con1 = from.as_mut_ptr().add(off1);
            let _off3 = write_con(from, off2, 2, &[con1]);
            
            let mut root = from.as_mut_ptr().add(off2);
            let roots = [&mut root as *mut *mut u8];
            let _res = cheney_copy(&roots, from.as_ptr(), from.as_ptr().add(1024), to);
            
            assert_eq!(root, to.as_mut_ptr());
            assert_eq!(read_tag(root), TAG_CON);
            assert_eq!(*(root.add(CON_TAG_OFFSET) as *const u64), 2);
            
            let c1 = *(root.add(CON_FIELDS_OFFSET) as *const *mut u8);
            assert_eq!(read_tag(c1), TAG_CON);
            assert_eq!(*(c1.add(CON_TAG_OFFSET) as *const u64), 1);
            
            let l1 = *(c1.add(CON_FIELDS_OFFSET) as *const *mut u8);
            assert_eq!(read_tag(l1), TAG_LIT);
            assert_eq!(*(l1.add(LIT_VALUE_OFFSET) as *const i64), 7);
        }
    }

    // 5. test_external_pointers_unchanged
    #[test]
    fn test_external_pointers_unchanged() {
        let mut from_buf = AlignedBuf([0u8; 1024]);
        let mut to_buf = AlignedBuf([0u8; 1024]);
        let from = &mut from_buf.0;
        let to = &mut to_buf.0;
        unsafe {
            let ext_ptr = 0x8899aabbccusize as *mut u8; // outside from_start..from_end
            let _off1 = write_closure(from, 0, 0x112233usize as *const u8, &[ext_ptr]);
            let mut root = from.as_mut_ptr();
            let roots = [&mut root as *mut *mut u8];
            let _res = cheney_copy(&roots, from.as_ptr(), from.as_ptr().add(1024), to);
            
            assert_eq!(root, to.as_mut_ptr());
            let cap = *(root.add(CLOSURE_CAPTURED_OFFSET) as *const *mut u8);
            assert_eq!(cap, ext_ptr); // remains unchanged
        }
    }

    // 6. test_diamond_sharing
    #[test]
    fn test_diamond_sharing() {
        let mut from_buf = AlignedBuf([0u8; 1024]);
        let mut to_buf = AlignedBuf([0u8; 1024]);
        let from = &mut from_buf.0;
        let to = &mut to_buf.0;
        unsafe {
            let _off1 = write_lit(from, 0, 42);
            let lit = from.as_mut_ptr();
            let mut root1 = lit;
            let mut root2 = lit;
            let roots = [&mut root1 as *mut *mut u8, &mut root2 as *mut *mut u8];
            
            let res = cheney_copy(&roots, from.as_ptr(), from.as_ptr().add(1024), to);
            
            assert_eq!(res.bytes_copied, LIT_SIZE); // copied only once
            assert_eq!(root1, root2);
            assert_eq!(root1, to.as_mut_ptr());
        }
    }

    // 7. test_dead_objects_not_copied
    #[test]
    fn test_dead_objects_not_copied() {
        let mut from_buf = AlignedBuf([0u8; 1024]);
        let mut to_buf = AlignedBuf([0u8; 1024]);
        let from = &mut from_buf.0;
        let to = &mut to_buf.0;
        unsafe {
            let off1 = write_lit(from, 0, 1);
            let off2 = write_lit(from, off1, 2); // this one is rooted
            let _off3 = write_lit(from, off2, 3);
            
            let mut root = from.as_mut_ptr().add(off1);
            let roots = [&mut root as *mut *mut u8];
            
            let res = cheney_copy(&roots, from.as_ptr(), from.as_ptr().add(1024), to);
            
            assert_eq!(res.bytes_copied, LIT_SIZE); // only 1 copied
            assert_eq!(read_tag(root), TAG_LIT);
            assert_eq!(*(root.add(LIT_VALUE_OFFSET) as *const i64), 2);
        }
    }

    // 8. test_for_each_pointer_field_lit
    #[test]
    fn test_for_each_pointer_field_lit() {
        let mut buf_data = AlignedBuf([0u8; 1024]);
        let buf = &mut buf_data.0;
        unsafe {
            write_lit(buf, 0, 10);
            let mut count = 0;
            for_each_pointer_field(buf.as_mut_ptr(), |_| {
                count += 1;
            });
            assert_eq!(count, 0);
        }
    }

    // 9. test_for_each_pointer_field_con
    #[test]
    fn test_for_each_pointer_field_con() {
        let mut buf_data = AlignedBuf([0u8; 1024]);
        let buf = &mut buf_data.0;
        unsafe {
            write_con(buf, 0, 1, &[0x1000 as *mut u8, 0x2000 as *mut u8]);
            let mut ptrs = Vec::new();
            for_each_pointer_field(buf.as_mut_ptr(), |p| {
                ptrs.push(*p);
            });
            assert_eq!(ptrs, vec![0x1000 as *mut u8, 0x2000 as *mut u8]);
        }
    }

    // 10. test_for_each_pointer_field_closure
    #[test]
    fn test_for_each_pointer_field_closure() {
        let mut buf_data = AlignedBuf([0u8; 1024]);
        let buf = &mut buf_data.0;
        unsafe {
            write_closure(buf, 0, 0x9999 as *const u8, &[0x3000 as *mut u8]);
            let mut ptrs = Vec::new();
            for_each_pointer_field(buf.as_mut_ptr(), |p| {
                ptrs.push(*p);
            });
            assert_eq!(ptrs, vec![0x3000 as *mut u8]); // code_ptr is excluded
        }
    }
}
