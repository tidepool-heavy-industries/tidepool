/// Discriminant tag for heap object types.
/// Stored at byte offset 0 of every HeapObject.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum HeapTag {
    Closure = 0,
    Thunk = 1,
    Con = 2,
    Lit = 3,
}

impl HeapTag {
    /// Convert from raw byte. Returns None for unknown tags.
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            0 => Some(HeapTag::Closure),
            1 => Some(HeapTag::Thunk),
            2 => Some(HeapTag::Con),
            3 => Some(HeapTag::Lit),
            _ => None,
        }
    }

    /// Convert to raw byte.
    pub fn as_byte(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for HeapTag {
    type Error = u8;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::from_byte(value).ok_or(value)
    }
}

impl std::fmt::Display for HeapTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HeapTag::Closure => f.write_str("Closure"),
            HeapTag::Thunk => f.write_str("Thunk"),
            HeapTag::Con => f.write_str("Con"),
            HeapTag::Lit => f.write_str("Lit"),
        }
    }
}

pub const TAG_CLOSURE: u8 = HeapTag::Closure as u8;
pub const TAG_THUNK: u8 = HeapTag::Thunk as u8;
pub const TAG_CON: u8 = HeapTag::Con as u8;
pub const TAG_LIT: u8 = HeapTag::Lit as u8;

/// Discriminant for thunk evaluation state.
/// Stored at the thunk state byte within a Thunk HeapObject.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ThunkStateTag {
    Unevaluated = 0,
    BlackHole = 1,
    Evaluated = 2,
}

impl ThunkStateTag {
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            0 => Some(ThunkStateTag::Unevaluated),
            1 => Some(ThunkStateTag::BlackHole),
            2 => Some(ThunkStateTag::Evaluated),
            _ => None,
        }
    }

    pub fn as_byte(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for ThunkStateTag {
    type Error = u8;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::from_byte(value).ok_or(value)
    }
}

impl std::fmt::Display for ThunkStateTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ThunkStateTag::Unevaluated => f.write_str("Unevaluated"),
            ThunkStateTag::BlackHole => f.write_str("BlackHole"),
            ThunkStateTag::Evaluated => f.write_str("Evaluated"),
        }
    }
}

pub const THUNK_UNEVALUATED: u8 = ThunkStateTag::Unevaluated as u8;
pub const THUNK_BLACKHOLE: u8 = ThunkStateTag::BlackHole as u8;
pub const THUNK_EVALUATED: u8 = ThunkStateTag::Evaluated as u8;

/// Discriminant for literal value types within a Lit HeapObject.
/// Stored at offset 8 (first byte after header).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum LitTag {
    Int = 0,
    Word = 1,
    Char = 2,
    Float = 3,
    Double = 4,
}

impl LitTag {
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            0 => Some(LitTag::Int),
            1 => Some(LitTag::Word),
            2 => Some(LitTag::Char),
            3 => Some(LitTag::Float),
            4 => Some(LitTag::Double),
            _ => None,
        }
    }

    pub fn as_byte(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for LitTag {
    type Error = u8;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::from_byte(value).ok_or(value)
    }
}

impl std::fmt::Display for LitTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LitTag::Int => f.write_str("Int#"),
            LitTag::Word => f.write_str("Word#"),
            LitTag::Char => f.write_str("Char#"),
            LitTag::Float => f.write_str("Float#"),
            LitTag::Double => f.write_str("Double#"),
        }
    }
}

// ── Payload field offsets ─────────────────────────────────────
// These match the HeapObject memory layout from decisions.md.
// All offsets are in bytes from the start of the HeapObject.

/// Offset of the tag byte (u8).
pub const OFFSET_TAG: usize = 0;
/// Offset of the size field (u16).
pub const OFFSET_SIZE: usize = 1;

// -- Closure layout (HeapTag::Closure) --
/// Offset of code_ptr (*const u8) in a Closure.
pub const CLOSURE_CODE_PTR_OFFSET: usize = 8;
/// Offset of num_captured (u16) in a Closure.
pub const CLOSURE_NUM_CAPTURED_OFFSET: usize = 16;
/// Offset of first captured variable pointer in a Closure.
pub const CLOSURE_CAPTURED_OFFSET: usize = 24;

// -- Con layout (HeapTag::Con) --
/// Offset of con_tag (u64, DataConId) in a Con.
pub const CON_TAG_OFFSET: usize = 8;
/// Offset of num_fields (u16) in a Con.
pub const CON_NUM_FIELDS_OFFSET: usize = 16;
/// Offset of first field pointer in a Con.
pub const CON_FIELDS_OFFSET: usize = 24;

// -- Thunk layout (HeapTag::Thunk) --
/// Offset of thunk_state byte in a Thunk.
pub const THUNK_STATE_OFFSET: usize = 8;
/// Offset of code_ptr in an Unevaluated Thunk.
pub const THUNK_CODE_PTR_OFFSET: usize = 16;
/// Offset of indirection pointer in an Evaluated Thunk (D7).
pub const THUNK_INDIRECTION_OFFSET: usize = 16;
/// Offset of first captured variable in a Thunk.
pub const THUNK_CAPTURED_OFFSET: usize = 24;

// -- Lit layout (HeapTag::Lit) --
/// Offset of lit_tag (LitTag byte) in a Lit.
pub const LIT_TAG_OFFSET: usize = 8;
/// Offset of the literal value (i64/u64/f64) in a Lit.
pub const LIT_VALUE_OFFSET: usize = 16;
/// Total size of a Lit HeapObject.
pub const LIT_SIZE: usize = 24;

/// Stride between consecutive pointer fields (8 bytes on 64-bit).
pub const FIELD_STRIDE: usize = 8;

/// tag(1) + size(2) + padding(5) = 8 bytes aligned
pub const HEADER_SIZE: usize = 8;

/// Read the tag byte from a heap object pointer.
///
/// # Safety
///
/// ptr must point to a valid HeapObject.
pub unsafe fn read_tag(ptr: *const u8) -> u8 {
    *ptr.add(OFFSET_TAG)
}

/// Read the tag byte and convert to HeapTag enum.
///
/// # Safety
///
/// ptr must point to a valid HeapObject.
pub unsafe fn read_heap_tag(ptr: *const u8) -> Option<HeapTag> {
    HeapTag::from_byte(*ptr.add(OFFSET_TAG))
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
    std::ptr::read_unaligned(ptr.add(OFFSET_SIZE) as *const u16)
}

/// Write tag + size header.
///
/// # Safety
///
/// ptr must point to allocated memory of at least `HEADER_SIZE` bytes.
/// size must be at least `HEADER_SIZE`.
pub unsafe fn write_header(ptr: *mut u8, tag: u8, size: u16) {
    *ptr.add(OFFSET_TAG) = tag;
    std::ptr::write_unaligned(ptr.add(OFFSET_SIZE) as *mut u16, size);
    // Padding bytes are from offset 3 to 7 (5 bytes).
    // Note: decisions.md says variant-specific payload follows at offset 3,
    // but also says all objects are 8-byte aligned.
    // If payload starts at offset 3, we should NOT zero these bytes.
    // However, the spec ALSO says: "tag(1) + size(2) + padding(5) = 8 bytes aligned"
    // in the Wave 1 description. We will follow the padding description for now
    // but only zero if size as usize >= HEADER_SIZE to be safe.
    if size as usize >= HEADER_SIZE {
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

    #[test]
    fn test_heap_tag_roundtrip() {
        for tag in [HeapTag::Closure, HeapTag::Thunk, HeapTag::Con, HeapTag::Lit] {
            assert_eq!(HeapTag::from_byte(tag.as_byte()), Some(tag));
            assert_eq!(HeapTag::try_from(tag.as_byte()), Ok(tag));
        }
        assert_eq!(HeapTag::from_byte(255), None);
        assert_eq!(HeapTag::try_from(255), Err(255));
    }

    #[test]
    fn test_heap_tag_constants_match() {
        assert_eq!(TAG_CLOSURE, HeapTag::Closure as u8);
        assert_eq!(TAG_THUNK, HeapTag::Thunk as u8);
        assert_eq!(TAG_CON, HeapTag::Con as u8);
        assert_eq!(TAG_LIT, HeapTag::Lit as u8);
    }

    #[test]
    fn test_thunk_state_tag_roundtrip() {
        for tag in [
            ThunkStateTag::Unevaluated,
            ThunkStateTag::BlackHole,
            ThunkStateTag::Evaluated,
        ] {
            assert_eq!(ThunkStateTag::from_byte(tag.as_byte()), Some(tag));
            assert_eq!(ThunkStateTag::try_from(tag.as_byte()), Ok(tag));
        }
        assert_eq!(ThunkStateTag::from_byte(255), None);
        assert_eq!(ThunkStateTag::try_from(255), Err(255));
    }

    #[test]
    fn test_thunk_state_constants_match() {
        assert_eq!(THUNK_UNEVALUATED, ThunkStateTag::Unevaluated as u8);
        assert_eq!(THUNK_BLACKHOLE, ThunkStateTag::BlackHole as u8);
        assert_eq!(THUNK_EVALUATED, ThunkStateTag::Evaluated as u8);
    }

    #[test]
    fn test_lit_tag_roundtrip() {
        for tag in [
            LitTag::Int,
            LitTag::Word,
            LitTag::Char,
            LitTag::Float,
            LitTag::Double,
        ] {
            assert_eq!(LitTag::from_byte(tag.as_byte()), Some(tag));
            assert_eq!(LitTag::try_from(tag.as_byte()), Ok(tag));
        }
        assert_eq!(LitTag::from_byte(255), None);
        assert_eq!(LitTag::try_from(255), Err(255));
    }

    #[test]
    fn test_heap_tag_display() {
        assert_eq!(HeapTag::Con.to_string(), "Con");
        assert_eq!(HeapTag::Lit.to_string(), "Lit");
    }

    #[test]
    fn test_offset_consistency() {
        // Verify layout assumptions
        assert_eq!(HEADER_SIZE, 8);
        assert_eq!(CLOSURE_CODE_PTR_OFFSET, HEADER_SIZE);
        assert_eq!(CON_TAG_OFFSET, HEADER_SIZE);
        assert_eq!(THUNK_STATE_OFFSET, HEADER_SIZE);
        assert_eq!(LIT_TAG_OFFSET, HEADER_SIZE);
        assert_eq!(FIELD_STRIDE, std::mem::size_of::<*const u8>());
    }
}
