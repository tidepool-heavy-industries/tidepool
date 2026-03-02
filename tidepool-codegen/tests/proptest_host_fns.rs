use proptest::prelude::*;
use std::alloc::{alloc_zeroed, dealloc, Layout};
use tidepool_codegen::host_fns::*;

/// Helper to create a byte slice from a string.
fn make_text_buffer(s: &str) -> Vec<u8> {
    s.as_bytes().to_vec()
}

/// Helper to create a ByteArray heap object for testing.
/// Layout: [u64 length][u8 bytes...]
fn make_byte_array(data: &[u8]) -> i64 {
    let size = data.len();
    let total = 8 + size;
    let layout = Layout::from_size_align(total, 8).unwrap();
    let ptr = unsafe { alloc_zeroed(layout) };
    if ptr.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
    unsafe {
        *(ptr as *mut u64) = size as u64;
        std::ptr::copy_nonoverlapping(data.as_ptr(), ptr.add(8), size);
    }
    ptr as i64
}

/// Helper to free a ByteArray heap object.
fn free_byte_array(ptr: i64) {
    let old_ptr = ptr as *mut u8;
    let size = unsafe { *(old_ptr as *const u64) } as usize;
    let layout = Layout::from_size_align(8 + size, 8).unwrap();
    unsafe { dealloc(old_ptr, layout) };
}

proptest! {
    /// For any valid UTF-8 string, measure_off(s, 0, len, i64::MAX) negated equals s.chars().count()
    #[test]
    fn length_matches_chars_count(s in any::<String>()) {
        let buf = make_text_buffer(&s);
        let res = runtime_text_measure_off(buf.as_ptr() as i64, 0, buf.len() as i64, i64::MAX);
        prop_assert_eq!(-res, s.chars().count() as i64);
    }

    /// For any string and n in 0..=char_count, measure_off(s, 0, len, n) returns the byte length of s.chars().take(n).collect::<String>()
    #[test]
    fn take_n_bytes_correct(s in any::<String>(), n in 0usize..1000usize) {
        let char_count = s.chars().count();
        let n = if char_count == 0 { 0 } else { n % (char_count + 1) };
        let buf = make_text_buffer(&s);
        let res = runtime_text_measure_off(buf.as_ptr() as i64, 0, buf.len() as i64, n as i64);

        let expected_str: String = s.chars().take(n).collect();
        let expected_bytes = expected_str.as_bytes().len() as i64;

        prop_assert_eq!(res, expected_bytes);
    }

    /// For any string and n, taking n chars + dropping n chars reconstructs the original char count
    #[test]
    fn take_then_drop_is_identity(s in any::<String>(), n in 0usize..1000usize) {
        let char_count = s.chars().count();
        let n = if char_count == 0 { 0 } else { n % (char_count + 1) };
        let buf = make_text_buffer(&s);

        let res_take_bytes = runtime_text_measure_off(buf.as_ptr() as i64, 0, buf.len() as i64, n as i64);
        let res_drop = runtime_text_measure_off(buf.as_ptr() as i64, res_take_bytes, (buf.len() as i64) - res_take_bytes, i64::MAX);

        prop_assert_eq!(n as i64 + (-res_drop), char_count as i64);
    }

    /// measure_off(s, off, len-off, n) agrees with measuring the suffix s[off..]
    #[test]
    fn offset_consistency(s in any::<String>(), off_chars in 0usize..1000usize, n in 0usize..1000usize) {
        let char_count = s.chars().count();
        let off_chars = if char_count == 0 { 0 } else { off_chars % (char_count + 1) };
        let buf = make_text_buffer(&s);

        // Find byte offset for off_chars
        let off_bytes = runtime_text_measure_off(buf.as_ptr() as i64, 0, buf.len() as i64, off_chars as i64);

        let res_suffix = runtime_text_measure_off(buf.as_ptr() as i64, off_bytes, (buf.len() as i64) - off_bytes, n as i64);

        let suffix_str: String = s.chars().skip(off_chars).collect();
        let suffix_buf = make_text_buffer(&suffix_str);
        let expected_res = runtime_text_measure_off(suffix_buf.as_ptr() as i64, 0, suffix_buf.len() as i64, n as i64);

        prop_assert_eq!(res_suffix, expected_res);
    }

    /// reverse(reverse(s)) == s for any valid UTF-8
    #[test]
    fn reverse_involution(s in any::<String>()) {
        let buf = make_text_buffer(&s);
        let mut dest1 = vec![0u8; buf.len()];
        let mut dest2 = vec![0u8; buf.len()];

        runtime_text_reverse(dest1.as_mut_ptr() as i64, buf.as_ptr() as i64, 0, buf.len() as i64);
        runtime_text_reverse(dest2.as_mut_ptr() as i64, dest1.as_ptr() as i64, 0, dest1.len() as i64);

        prop_assert_eq!(dest2, buf);
    }

    /// reverse(s) byte-equals s.chars().rev().collect::<String>() for any valid UTF-8
    #[test]
    fn reverse_matches_std_reverse(s in any::<String>()) {
        let buf = make_text_buffer(&s);
        let mut dest = vec![0u8; buf.len()];

        runtime_text_reverse(dest.as_mut_ptr() as i64, buf.as_ptr() as i64, 0, buf.len() as i64);

        let expected: String = s.chars().rev().collect();
        prop_assert_eq!(dest, expected.as_bytes());
    }

    /// reversed output has same byte length as input
    #[test]
    fn reverse_preserves_length(s in any::<String>()) {
        let buf = make_text_buffer(&s);
        let mut dest = vec![0u8; buf.len()];

        runtime_text_reverse(dest.as_mut_ptr() as i64, buf.as_ptr() as i64, 0, buf.len() as i64);

        prop_assert_eq!(dest.len(), buf.len());
    }

    /// if memchr returns idx >= 0, then src[idx] == byte
    #[test]
    fn memchr_found_means_byte_matches(buf in any::<Vec<u8>>(), byte in any::<u8>()) {
        let res = runtime_text_memchr(buf.as_ptr() as i64, 0, buf.len() as i64, byte as i64);
        if res >= 0 {
            prop_assert_eq!(buf[res as usize], byte);
        }
    }

    /// if memchr returns -1, then byte is not in src[off..off+len]
    #[test]
    fn memchr_not_found_means_absent(buf in any::<Vec<u8>>(), byte in any::<u8>()) {
        let res = runtime_text_memchr(buf.as_ptr() as i64, 0, buf.len() as i64, byte as i64);
        if res == -1 {
            prop_assert!(!buf.contains(&byte));
        }
    }

    /// if found, no earlier position in [off..idx) contains that byte
    #[test]
    fn memchr_finds_first_occurrence(buf in any::<Vec<u8>>(), byte in any::<u8>()) {
        let res = runtime_text_memchr(buf.as_ptr() as i64, 0, buf.len() as i64, byte as i64);
        if res >= 0 {
            for i in 0..(res as usize) {
                prop_assert_ne!(buf[i], byte);
            }
        }
    }

    /// compare byte arrays matches Ord on &[u8]
    #[test]
    fn compare_byte_arrays_matches_ord(a_vec in any::<Vec<u8>>(), b_vec in any::<Vec<u8>>()) {
        let len = a_vec.len().min(b_vec.len());
        let a_ba = make_byte_array(&a_vec);
        let b_ba = make_byte_array(&b_vec);

        let res = runtime_compare_byte_arrays(a_ba, 0, b_ba, 0, len as i64);

        let expected = match a_vec[..len].cmp(&b_vec[..len]) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        };

        free_byte_array(a_ba);
        free_byte_array(b_ba);

        prop_assert_eq!(res, expected);
    }
}
