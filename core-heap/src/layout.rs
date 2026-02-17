pub const TAG_CLOSURE: u8 = 0;
pub const TAG_THUNK: u8 = 1;
pub const TAG_CON: u8 = 2;
pub const TAG_LIT: u8 = 3;

pub const THUNK_UNEVALUATED: u8 = 0;
pub const THUNK_BLACKHOLE: u8 = 1;
pub const THUNK_EVALUATED: u8 = 2;

/// tag(1) + size(2) + padding(5) = 8 bytes aligned
pub const HEADER_SIZE: usize = 8;

/// Read the tag byte from a heap object pointer.
///
/// # Safety
///
/// ptr must point to a valid HeapObject.
pub unsafe fn read_tag(ptr: *const u8) -> u8 {
    *ptr
}

/// Read the total size from a heap object pointer.
///
/// # Safety
///
/// ptr must point to a valid HeapObject.
pub unsafe fn read_size(ptr: *const u8) -> u16 {
    // Size is stored at offset 1 as u16.
    // Use read_unaligned in case the pointer itself is not perfectly aligned
    // (though our objects should be 8-byte aligned).
    std::ptr::read_unaligned(ptr.add(1) as *const u16)
}

/// Write tag + size header.
///
/// # Safety
///
/// ptr must point to allocated memory of at least `HEADER_SIZE` bytes.
/// size must be at least `HEADER_SIZE`.
pub unsafe fn write_header(ptr: *mut u8, tag: u8, size: u16) {
    *ptr = tag;
    std::ptr::write_unaligned(ptr.add(1) as *mut u16, size);
    // Padding bytes are from offset 3 to 7 (5 bytes).
    // Note: decisions.md says variant-specific payload follows at offset 3,
    // but also says all objects are 8-byte aligned.
    // If payload starts at offset 3, we should NOT zero these bytes.
    // However, the spec ALSO says: "tag(1) + size(2) + padding(5) = 8 bytes aligned"
    // in the Wave 1 description. We will follow the padding description for now
    // but only zero if size >= 8 to be safe.
    if size >= 8 {
        std::ptr::write_bytes(ptr.add(3), 0, 5);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_header_roundtrip() {
        let mut buffer = [0u8; HEADER_SIZE];
        let ptr = buffer.as_mut_ptr();

        unsafe {
            write_header(ptr, TAG_THUNK, 16);
            assert_eq!(read_tag(ptr), TAG_THUNK);
            assert_eq!(read_size(ptr), 16);
        }
    }

    #[test]
    fn test_alignment_roundtrip() {
        // Only header is written, so 8-byte buffer is enough for the header.
        // We test various logical sizes to ensure the size u16 can store them.
        let sizes = [8, 16, 64, 256, 1024, 65535];
        for &size in &sizes {
            let mut buffer = [0u8; 8];
            let ptr = buffer.as_mut_ptr();
            unsafe {
                write_header(ptr, TAG_CON, size);
                assert_eq!(read_tag(ptr), TAG_CON);
                assert_eq!(read_size(ptr), size);
            }
        }
    }

    #[test]
    fn test_header_offset() {
        // Header size should be 8.
        assert_eq!(HEADER_SIZE, 8);
    }
}
