//! Probing raw heap object layout primitives for encoding bugs.
//!
//! Surface inventory:
//! - tidepool_heap::layout: write_header, read_tag, read_size, constants (TAG_*, OFFSET_*, *_OFFSET)
//! - tidepool_heap::gc::raw: for_each_pointer_field, evacuate via cheney_copy
//! - tidepool_codegen::layout: constants (synchronization check, LIT_TAG_*)
//! - tidepool_codegen::host_fns: runtime_new_byte_array, runtime_shrink_byte_array, runtime_resize_byte_array, runtime_set_byte_array, error_poison_ptr

use proptest::prelude::*;
use std::alloc::{alloc_zeroed, dealloc, Layout};
use tidepool_codegen::host_fns::*;
use tidepool_codegen::layout as cg_layout;
use tidepool_heap::gc::raw::{cheney_copy, for_each_pointer_field};
use tidepool_heap::layout::*;

// --- Helpers ---

struct AllocGuard {
    ptr: *mut u8,
    layout: Layout,
}

impl Drop for AllocGuard {
    fn drop(&mut self) {
        unsafe { dealloc(self.ptr, self.layout) };
    }
}

fn alloc_buf(size: usize) -> AllocGuard {
    let layout = Layout::from_size_align(size, 8).unwrap();
    let ptr = unsafe { alloc_zeroed(layout) };
    if ptr.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
    AllocGuard { ptr, layout }
}

/// Mirrors the production pattern: write_header(ptr, TAG_CON, (24 + 8*len) as u16)
unsafe fn write_con_raw(ptr: *mut u8, con_tag: u64, fields: &[*mut u8]) {
    let len = fields.len();
    let size = (24 + 8 * len) as u16; // Deliberate cast to mirror production
    write_header(ptr, TAG_CON, size);
    *(ptr.add(CON_TAG_OFFSET) as *mut u64) = con_tag;
    *(ptr.add(CON_NUM_FIELDS_OFFSET) as *mut u16) = len as u16;
    for (i, &field) in fields.iter().enumerate() {
        *(ptr.add(CON_FIELDS_OFFSET + i * 8) as *mut *mut u8) = field;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ForkVerdict {
    Pass,
    Signal(i32),
    Exit(i32),
}

fn fork_contained<F: FnOnce()>(f: F) -> ForkVerdict {
    unsafe {
        let pid = libc::fork();
        if pid < 0 {
            panic!("fork failed");
        }
        if pid == 0 {
            // Child
            f();
            libc::_exit(0);
        } else {
            // Parent
            let mut status = 0;
            libc::waitpid(pid, &mut status, 0);
            if libc::WIFSIGNALED(status) {
                ForkVerdict::Signal(libc::WTERMSIG(status))
            } else if libc::WIFEXITED(status) {
                let code = libc::WEXITSTATUS(status);
                if code == 0 {
                    ForkVerdict::Pass
                } else {
                    ForkVerdict::Exit(code)
                }
            } else {
                ForkVerdict::Exit(-1)
            }
        }
    }
}

// --- Property Groups ---

mod group1_header {
    use super::*;

    #[test]
    fn test_header_fenceposts() {
        let sizes = [0, 1, 8, 255, 256, 257, 4095, 65534, 65535];
        let tags = [TAG_CLOSURE, TAG_THUNK, TAG_CON, TAG_LIT, TAG_FORWARDED];
        for &tag in &tags {
            for &size in &sizes {
                let guard = alloc_buf(65536);
                unsafe {
                    write_header(guard.ptr, tag, size as u16);
                    assert_eq!(read_tag(guard.ptr), tag, "Tag mismatch for size {}", size);
                    assert_eq!(
                        read_size(guard.ptr),
                        size as u16,
                        "Size mismatch for size {}",
                        size
                    );
                }
            }
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 400, ..ProptestConfig::default() })]
        #[test]
        fn prop_header_roundtrip(tag in 0..=255u8, size in 0..=65535u16) {
            let guard = alloc_buf(65536);
            unsafe {
                write_header(guard.ptr, tag, size);
                prop_assert_eq!(read_tag(guard.ptr), tag);
                prop_assert_eq!(read_size(guard.ptr), size);
            }
        }
    }

    #[test]
    fn test_padding_asymmetry() {
        let sizes_to_test = [0, 1, 7, 8, 255];
        for &size in &sizes_to_test {
            let layout = Layout::from_size_align(8, 8).unwrap();
            let ptr = unsafe { std::alloc::alloc(layout) };
            unsafe {
                std::ptr::write_bytes(ptr, 0xAA, 8);
                write_header(ptr, TAG_CON, size as u16);
                let padding = std::slice::from_raw_parts(ptr.add(3), 5);
                if size >= 8 {
                    assert_eq!(
                        padding,
                        &[0, 0, 0, 0, 0],
                        "Padding should be zeroed for size {}",
                        size
                    );
                } else {
                    assert_eq!(
                        padding,
                        &[0xAA, 0xAA, 0xAA, 0xAA, 0xAA],
                        "Padding should NOT be zeroed for size {}",
                        size
                    );
                }
                dealloc(ptr, layout);
            }
        }
    }
}

mod group2_con {
    use super::*;

    #[test]
    fn test_con_field_identity() {
        let field_counts = [0, 1, 2, 1023, 1024, 8188];
        for &count in &field_counts {
            let byte_size = 24 + 8 * count;
            let guard = alloc_buf(byte_size);
            let fields: Vec<*mut u8> = (0..count).map(|i| i as *mut u8).collect();
            unsafe {
                write_con_raw(guard.ptr, 0xDEADBEEF, &fields);
                assert_eq!(read_tag(guard.ptr), TAG_CON);
                assert_eq!(read_size(guard.ptr), byte_size as u16);
                let read_con_tag = *(guard.ptr.add(CON_TAG_OFFSET) as *const u64);
                assert_eq!(read_con_tag, 0xDEADBEEF);
                let read_num_fields = *(guard.ptr.add(CON_NUM_FIELDS_OFFSET) as *const u16);
                assert_eq!(read_num_fields, count as u16);
                for i in 0..count {
                    let field_ptr = *(guard.ptr.add(CON_FIELDS_OFFSET + i * 8) as *const *mut u8);
                    assert_eq!(field_ptr, i as *mut u8);
                }
            }
        }
    }

    #[test]
    fn test_con_65535_fields() {
        // Only if ~512KB malloc per case is acceptable: do it as a #[test], not inside proptest.
        let count = 65535;
        let byte_size = 24 + 8 * count; // 524304 bytes
        let guard = alloc_buf(byte_size);
        unsafe {
            let size_cast = (24 + 8 * count) as u16; // 524304 % 65536 = 16
            write_header(guard.ptr, TAG_CON, size_cast);
            *(guard.ptr.add(CON_TAG_OFFSET) as *mut u64) = 0xCAFE;
            *(guard.ptr.add(CON_NUM_FIELDS_OFFSET) as *mut u16) = count as u16;

            assert_eq!(read_tag(guard.ptr), TAG_CON);
            assert_eq!(read_size(guard.ptr), 16); // SILENT WRAP
            assert_eq!(*(guard.ptr.add(CON_NUM_FIELDS_OFFSET) as *const u16), 65535);
        }
    }
}

mod group3_lit {
    use super::*;

    #[test]
    fn test_lit_tag_drift() {
        // codegen defines LIT_TAG_STRING=5...ARRAY=9, heap only goes to 4.
        for tag_val in 0..=9u8 {
            let guard = alloc_buf(LIT_SIZE);
            unsafe {
                write_header(guard.ptr, TAG_LIT, LIT_SIZE as u16);
                *guard.ptr.add(LIT_TAG_OFFSET) = tag_val;
                let heap_tag = LitTag::from_byte(tag_val);
                if tag_val <= 4 {
                    assert!(heap_tag.is_some());
                } else {
                    assert!(
                        heap_tag.is_none(),
                        "Tag {} should be None in heap-side LitTag",
                        tag_val
                    );
                }
            }
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 400, ..ProptestConfig::default() })]
        #[test]
        fn prop_lit_nan_preservation(bits in any::<u64>()) {
            let guard = alloc_buf(LIT_SIZE);
            unsafe {
                write_header(guard.ptr, TAG_LIT, LIT_SIZE as u16);
                *guard.ptr.add(LIT_TAG_OFFSET) = LitTag::Double as u8;
                *(guard.ptr.add(LIT_VALUE_OFFSET) as *mut u64) = bits;
                let read_bits = *(guard.ptr.add(LIT_VALUE_OFFSET) as *const u64);
                prop_assert_eq!(read_bits, bits);
            }
        }
    }

    #[test]
    fn test_lit_shared_constants() {
        assert_eq!(CON_TAG_OFFSET, cg_layout::CON_TAG_OFFSET as usize);
        assert_eq!(
            CON_NUM_FIELDS_OFFSET,
            cg_layout::CON_NUM_FIELDS_OFFSET as usize
        );
        assert_eq!(CON_FIELDS_OFFSET, cg_layout::CON_FIELDS_OFFSET as usize);
        assert_eq!(
            CLOSURE_CODE_PTR_OFFSET,
            cg_layout::CLOSURE_CODE_PTR_OFFSET as usize
        );
        assert_eq!(
            CLOSURE_NUM_CAPTURED_OFFSET,
            cg_layout::CLOSURE_NUM_CAPTURED_OFFSET as usize
        );
        assert_eq!(
            CLOSURE_CAPTURED_OFFSET,
            cg_layout::CLOSURE_CAPTURED_OFFSET as usize
        );
        assert_eq!(THUNK_STATE_OFFSET, cg_layout::THUNK_STATE_OFFSET as usize);
        assert_eq!(
            THUNK_CODE_PTR_OFFSET,
            cg_layout::THUNK_CODE_PTR_OFFSET as usize
        );
        assert_eq!(
            THUNK_CAPTURED_OFFSET,
            cg_layout::THUNK_CAPTURED_OFFSET as usize
        );
        assert_eq!(
            THUNK_INDIRECTION_OFFSET,
            cg_layout::THUNK_INDIRECTION_OFFSET as usize
        );
        assert_eq!(LIT_TAG_OFFSET, cg_layout::LIT_TAG_OFFSET as usize);
        assert_eq!(LIT_VALUE_OFFSET, cg_layout::LIT_VALUE_OFFSET as usize);
        assert_eq!(LIT_SIZE, cg_layout::LIT_TOTAL_SIZE as usize);
        assert_eq!(HEADER_SIZE, cg_layout::HEAP_HEADER_SIZE as usize);
    }
}

mod group4_bytearray {
    use super::*;

    #[test]
    fn test_byte_array_new_fenceposts() {
        let sizes = [0, 1, 7, 8, 63, 64, 65];
        for &n in &sizes {
            let ba = runtime_new_byte_array(n as i64);
            unsafe {
                assert_eq!(ba % 8, 0);
                assert_eq!(*(ba as *const u64), n as u64);
                let total = *((ba - 8) as *const u64);
                assert_eq!(total, (16 + n) as u64);
                let data = std::slice::from_raw_parts((ba + 8) as *const u8, n);
                for &b in data {
                    assert_eq!(b, 0);
                }
                // Cleanup
                let layout = Layout::from_size_align(total as usize, 8).unwrap();
                dealloc((ba - 8) as *mut u8, layout);
            }
        }
    }

    #[test]
    fn test_byte_array_new_poison() {
        let ba = runtime_new_byte_array(-1);
        assert_eq!(ba, error_poison_ptr() as i64);
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 400, ..ProptestConfig::default() })]
        #[test]
        fn prop_byte_array_shrink_sequence(n_initial in 0..1024usize, shrinks in prop::collection::vec(0..1024usize, 1..10)) {
            let ba = runtime_new_byte_array(n_initial as i64);
            let mut current_logical = n_initial;
            unsafe {
                let initial_total = *((ba - 8) as *const u64);
                for &target in &shrinks {
                    runtime_shrink_byte_array(ba, target as i64);
                    if target <= current_logical {
                        current_logical = target;
                    }
                    prop_assert_eq!(*(ba as *const u64), current_logical as u64);
                    prop_assert_eq!(*((ba - 8) as *const u64), initial_total);
                }
                // Cleanup
                let layout = Layout::from_size_align(initial_total as usize, 8).unwrap();
                dealloc((ba - 8) as *mut u8, layout);
            }
        }
    }

    #[test]
    fn test_byte_array_resize_content_preservation() {
        let n = 100;
        let ba = runtime_new_byte_array(n);
        unsafe {
            for i in 0..n {
                *((ba + 8 + i) as *mut u8) = i as u8;
            }
            // Grow
            let new_n = 200;
            let new_ba = runtime_resize_byte_array(ba, new_n);
            assert_eq!(*(new_ba as *const u64), new_n as u64);
            assert_eq!(*((new_ba - 8) as *const u64), (16 + new_n) as u64);
            for i in 0..n {
                assert_eq!(*((new_ba + 8 + i) as *const u8), i as u8);
            }
            for i in n..new_n {
                assert_eq!(*((new_ba + 8 + i) as *const u8), 0);
            }

            // Shrink via resize
            let newer_n = 50;
            let newer_ba = runtime_resize_byte_array(new_ba, newer_n);

            assert_eq!(*(newer_ba as *const u64), newer_n as u64);
            for i in 0..newer_n {
                assert_eq!(*((newer_ba + 8 + i) as *const u8), i as u8);
            }

            // Cleanup
            let total = *((newer_ba - 8) as *const u64);
            let layout = Layout::from_size_align(total as usize, 8).unwrap();
            dealloc((newer_ba - 8) as *mut u8, layout);
        }
    }

    #[test]
    fn test_byte_array_resize_from_zero() {
        let ba = runtime_new_byte_array(0);
        let new_ba = runtime_resize_byte_array(ba, 10);
        unsafe {
            assert_eq!(*(new_ba as *const u64), 10);
            let total = *((new_ba - 8) as *const u64);
            let layout = Layout::from_size_align(total as usize, 8).unwrap();
            dealloc((new_ba - 8) as *mut u8, layout);
        }
    }

    #[test]
    fn test_byte_array_free_exactly_once() {
        // Chain k resizes in fork
        let verdict = fork_contained(|| {
            let mut ba = runtime_new_byte_array(10);
            for i in 0..10 {
                ba = runtime_resize_byte_array(ba, (i + 11) as i64);
            }
            unsafe {
                let total = *((ba - 8) as *const u64);
                let layout = Layout::from_size_align(total as usize, 8).unwrap();
                dealloc((ba - 8) as *mut u8, layout);
            }
        });
        assert_eq!(verdict, ForkVerdict::Pass);
    }
}

mod group5_thunk {
    use super::*;

    #[test]
    fn test_thunk_state_roundtrip() {
        let states = [THUNK_UNEVALUATED, THUNK_BLACKHOLE, THUNK_EVALUATED];
        for &state in &states {
            let tag = ThunkStateTag::from_byte(state);
            assert!(tag.is_some());
            assert_eq!(tag.unwrap().as_byte(), state);
        }
        for b in 3..=255u8 {
            assert!(ThunkStateTag::from_byte(b).is_none());
        }
    }

    #[test]
    fn test_thunk_pointer_fields_unevaluated() {
        let ncaps = 5;
        let byte_size = 24 + 8 * ncaps;
        let guard = alloc_buf(byte_size);
        unsafe {
            write_header(guard.ptr, TAG_THUNK, byte_size as u16);
            *guard.ptr.add(THUNK_STATE_OFFSET) = THUNK_UNEVALUATED;
            let mut count = 0;
            for_each_pointer_field(guard.ptr, |_| count += 1);
            assert_eq!(count, ncaps);
        }
    }

    #[test]
    fn test_thunk_pointer_fields_evaluated() {
        let ncaps = 5;
        let byte_size = 24 + 8 * ncaps;
        let guard = alloc_buf(byte_size);
        unsafe {
            write_header(guard.ptr, TAG_THUNK, byte_size as u16);
            *guard.ptr.add(THUNK_STATE_OFFSET) = THUNK_EVALUATED;
            let mut count = 0;
            for_each_pointer_field(guard.ptr, |_| count += 1);
            assert_eq!(count, 1); // Only indirection pointer at offset 16
        }
    }

    #[test]
    fn test_thunk_pointer_fields_blackhole() {
        let ncaps = 5;
        let byte_size = 24 + 8 * ncaps;
        let guard = alloc_buf(byte_size);
        unsafe {
            write_header(guard.ptr, TAG_THUNK, byte_size as u16);
            *guard.ptr.add(THUNK_STATE_OFFSET) = THUNK_BLACKHOLE;
            let mut count = 0;
            for_each_pointer_field(guard.ptr, |_| count += 1);
            assert_eq!(count, 0);
        }
    }

    #[test]
    fn test_blackhole_gc_invisibility() {
        let verdict = fork_contained(|| {
            let from_guard = alloc_buf(1024);
            let to_guard = alloc_buf(1024);
            unsafe {
                // 1. Write a Lit in from-space
                let lit_ptr = from_guard.ptr;
                write_header(lit_ptr, TAG_LIT, LIT_SIZE as u16);

                // 2. Write a BlackHole Thunk in from-space capturing the Lit
                let thunk_ptr = from_guard.ptr.add(LIT_SIZE);
                let thunk_size = 24 + 8;
                write_header(thunk_ptr, TAG_THUNK, thunk_size as u16);
                *thunk_ptr.add(THUNK_STATE_OFFSET) = THUNK_BLACKHOLE;
                *(thunk_ptr.add(THUNK_CAPTURED_OFFSET) as *mut *mut u8) = lit_ptr;

                // 3. Cheney copy with the thunk as root
                let mut root = thunk_ptr;
                let roots = [&mut root as *mut *mut u8];
                let to_space = std::slice::from_raw_parts_mut(to_guard.ptr, 1024);
                cheney_copy(&roots, from_guard.ptr, from_guard.ptr.add(1024), to_space);

                // 4. Verify thunk was copied but Lit was NOT
                assert_ne!(root, thunk_ptr); // thunk moved
                let captured_in_new = *(root.add(THUNK_CAPTURED_OFFSET) as *const *mut u8);
                assert_eq!(
                    captured_in_new, lit_ptr,
                    "Lit should NOT have been evacuated"
                );
            }
        });
        assert_eq!(verdict, ForkVerdict::Pass);
    }
}

mod bug_repros {
    use super::*;

    #[test]
    #[ignore = "BUG: C1 silent wrap in write_header"]
    fn bug_c1_header_wrap() {
        let guard = alloc_buf(8);
        unsafe {
            let size: usize = 65536;
            write_header(guard.ptr, TAG_CON, size as u16);
            let read = read_size(guard.ptr);
            assert_eq!(read, 0, "Expected 0 due to wrap, but got {}", read);
        }
    }

    #[test]
    #[ignore = "BUG: C2 Con writer silent wrap and GC corruption"]
    fn bug_c2_con_writer_wrap_gc() {
        let verdict = fork_contained(|| {
            let from_guard = alloc_buf(70000);
            let to_guard = alloc_buf(70000);
            unsafe {
                // Con with 8189 fields => size 24 + 8*8189 = 65536 => 0 as u16
                let count = 8189;
                let fields: Vec<*mut u8> = (0..count).map(|_| 0x1234 as *mut u8).collect();
                write_con_raw(from_guard.ptr, 0xCAFE, &fields);

                assert_eq!(read_size(from_guard.ptr), 0);

                let mut root = from_guard.ptr;
                let roots = [&mut root as *mut *mut u8];
                let to_space = std::slice::from_raw_parts_mut(to_guard.ptr, 70000);
                let res = cheney_copy(&roots, from_guard.ptr, from_guard.ptr.add(70000), to_space);

                assert_eq!(
                    res.bytes_copied, 0,
                    "Evacuate copied {} bytes but object has 8189 fields",
                    res.bytes_copied
                );
            }
        });
        assert_eq!(verdict, ForkVerdict::Pass);
    }
}
