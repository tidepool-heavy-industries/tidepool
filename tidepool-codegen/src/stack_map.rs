use std::collections::BTreeMap;

/// Information about GC roots at a single safepoint.
#[derive(Debug, Clone)]
pub struct StackMapInfo {
    /// Size of the frame in bytes (span from user_stack_maps tuple).
    pub frame_size: u32,
    /// SP-relative offsets of heap pointer slots.
    /// root_addr = SP + offset at the safepoint.
    pub offsets: Vec<u32>,
}

pub type RawStackMapEntry = (cranelift_codegen::ir::types::Type, u32);
pub type RawStackMap = (u32, u32, Vec<RawStackMapEntry>);

/// Maps absolute return addresses to stack map info.
///
/// Key = function_base_ptr + code_offset
/// (i.e., the return address, which is what the frame walker sees as caller_pc).
/// Cranelift's `code_offset` for user stack maps already points to the
/// instruction AFTER the call (the return point).
#[derive(Debug, Default)]
pub struct StackMapRegistry {
    entries: BTreeMap<usize, StackMapInfo>,
    /// Known JIT function address ranges (start, end).
    ranges: Vec<(usize, usize)>,
}

impl StackMapRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register stack map entries from a compiled function.
    ///
    /// `base_ptr` is the start address of the compiled function in memory.
    /// `size` is the total size of the function in bytes.
    /// `raw_entries` come from `CompiledCode.buffer.user_stack_maps()`:
    ///   each tuple is (code_offset, frame_size, UserStackMap).
    ///
    /// We key by `base_ptr + code_offset` as the return address. Cranelift's
    /// `code_offset` for user stack maps points to the instruction AFTER the call
    /// (the return point), so `base_ptr + code_offset` IS the absolute return address.
    pub fn register(&mut self, base_ptr: usize, size: u32, raw_entries: &[RawStackMap]) {
        self.ranges.push((base_ptr, base_ptr + size as usize));

        for (code_offset, frame_size, slot_entries) in raw_entries {
            let return_addr = base_ptr + *code_offset as usize;
            let offsets: Vec<u32> = slot_entries.iter().map(|(_, offset)| *offset).collect();
            self.entries.insert(
                return_addr,
                StackMapInfo {
                    frame_size: *frame_size,
                    offsets,
                },
            );
        }
    }

    /// Look up stack map info by return address (PC value from frame walker).
    pub fn lookup(&self, return_addr: usize) -> Option<&StackMapInfo> {
        self.entries.get(&return_addr)
    }

    /// Number of registered safepoints.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Check if an address falls within the known JIT code region.
    /// Used by the frame walker to determine when to stop walking.
    pub fn contains_address(&self, addr: usize) -> bool {
        self.ranges
            .iter()
            .any(|(start, end)| addr >= *start && addr < *end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stack_map_contains_address_boundaries() {
        let mut registry = StackMapRegistry::new();
        let start = 0x1000;
        let size = 0x100;
        let end = start + size;

        registry.register(start, size as u32, &[]);

        // 1. addr == start → should return true (inclusive start)
        assert!(
            registry.contains_address(start),
            "Address at exactly 'start' should be contained"
        );

        // 2. addr == end - 1 → should return true (last byte in range)
        assert!(
            registry.contains_address(end - 1),
            "Address at 'end - 1' should be contained"
        );

        // 3. addr == end → should return false (exclusive end)
        assert!(
            !registry.contains_address(end),
            "Address at exactly 'end' should NOT be contained"
        );

        // 4. addr == start - 1 → should return false (one byte before start)
        assert!(
            !registry.contains_address(start - 1),
            "Address at 'start - 1' should NOT be contained"
        );
    }
}
