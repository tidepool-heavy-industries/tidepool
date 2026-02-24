/// Test to validate the hypothesis that `runtime_text_measure_off` needs to return
/// a negated character count when requested by Haskell Core's `T.length`.
#[cfg(test)]
mod tests {
    /// Simplified implementation of the hypothesized fix for `runtime_text_measure_off`.
    fn validate_text_measure_off(addr: *const u8, off: i64, len: i64, n: i64) -> i64 {
        let ptr = unsafe { addr.add(off as usize) };
        let end = unsafe { ptr.add(len as usize) };
        let mut p = ptr;

        if n < 0 || n == i64::MAX {
            // Count characters in the first `len` bytes.
            // Return number of characters, negated.
            let mut count = 0i64;
            while p < end {
                let b = unsafe { *p };
                let char_len = if b < 0x80 {
                    1
                } else if b < 0xE0 {
                    2
                } else if b < 0xF0 {
                    3
                } else {
                    4
                };
                p = unsafe { p.add(char_len) };
                count += 1;
            }
            -count
        } else {
            // Measure how many bytes are in the first `n` characters,
            // but don't go past `len` bytes.
            let mut chars_to_count = n;
            while chars_to_count > 0 && p < end {
                let b = unsafe { *p };
                let char_len = if b < 0x80 {
                    1
                } else if b < 0xE0 {
                    2
                } else if b < 0xF0 {
                    3
                } else {
                    4
                };
                p = unsafe { p.add(char_len) };
                chars_to_count -= 1;
            }
            unsafe { p.offset_from(ptr) as i64 }
        }
    }

    #[test]
    fn test_length_negation_logic() {
        let s = "hello".as_bytes();
        let addr = s.as_ptr();
        
        // Haskell calls with n = i64::MAX for T.length
        let n = i64::MAX;
        let result = validate_text_measure_off(addr, 0, 5, n);
        
        // It should return -5 (negated character count)
        assert_eq!(result, -5, "Should return -5 for 5 ASCII characters when n=MAX");
        
        // The final Haskell result (as seen in Core) will be negateInt# (-5) = 5.
        assert_eq!(-result, 5, "Haskell's negation should yield 5");
    }

    #[test]
    fn test_take_byte_logic() {
        let s = "hello".as_bytes();
        let addr = s.as_ptr();
        
        // T.take 3 "hello" should call with n=3
        let n = 3;
        let result = validate_text_measure_off(addr, 0, 5, n);
        
        // It should return 3 (the number of bytes for 3 characters)
        assert_eq!(result, 3, "Should return 3 bytes for 3 characters");
    }

    #[test]
    fn test_multi_byte_length() {
        // "λ" is 2 bytes: CF BB
        // "😀" is 4 bytes: F0 9F 98 80
        let s = "λ😀x".as_bytes(); // Total 2+4+1 = 7 bytes, 3 characters
        let addr = s.as_ptr();
        
        let n = i64::MAX;
        let result = validate_text_measure_off(addr, 0, 7, n);
        
        assert_eq!(result, -3, "Should return -3 for 3 characters (negated)");
        assert_eq!(-result, 3, "Haskell's negation should yield 3");
    }

    #[test]
    fn test_multi_byte_take() {
        let s = "λ😀x".as_bytes(); // Total 2+4+1 = 7 bytes, 3 characters
        let addr = s.as_ptr();
        
        // T.take 2 "λ😀x" should call with n=2
        let n = 2;
        let result = validate_text_measure_off(addr, 0, 7, n);
        
        // It should return 6 (2 bytes for λ + 4 bytes for 😀)
        assert_eq!(result, 6, "Should return 6 bytes for 2 multi-byte characters");
    }
}
