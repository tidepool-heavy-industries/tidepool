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

/// Maps absolute return addresses to stack map info.
///
/// Key = function_base_ptr + code_offset + call_instruction_size
/// (i.e., the return address, which is what the frame walker sees as caller_pc).
#[derive(Debug, Default)]
pub struct StackMapRegistry {
    entries: BTreeMap<usize, StackMapInfo>,
}

impl StackMapRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register stack map entries from a compiled function.
    ///
    /// `base_ptr` is the start address of the compiled function in memory.
    /// `raw_entries` come from `CompiledCode.buffer.user_stack_maps()`:
    ///   each tuple is (code_offset, frame_size, UserStackMap).
    ///
    /// We key by `base_ptr + code_offset` as an approximation of the return address.
    /// The exact return address depends on the call instruction encoding size,
    /// but Cranelift's code_offset for user stack maps points to the instruction
    /// AFTER the call (the return point), so base_ptr + code_offset IS the return address.
    pub fn register(&mut self, base_ptr: usize, raw_entries: &[(u32, u32, Vec<(cranelift_codegen::ir::types::Type, u32)>)]) {
        for (code_offset, frame_size, slot_entries) in raw_entries {
            let return_addr = base_ptr + *code_offset as usize;
            let offsets: Vec<u32> = slot_entries.iter().map(|(_, offset)| *offset).collect();
            self.entries.insert(return_addr, StackMapInfo {
                frame_size: *frame_size,
                offsets,
            });
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
        // If we have any entries, check if addr is in the range
        // of known return addresses. This is a rough heuristic;
        // a more precise approach would track function start/end ranges.
        if let (Some((&min, _)), Some((&max, _))) = (self.entries.iter().next(), self.entries.iter().next_back()) {
            // Give some slack for the function boundaries
            addr >= min.saturating_sub(4096) && addr <= max.saturating_add(4096)
        } else {
            false
        }
    }
}
