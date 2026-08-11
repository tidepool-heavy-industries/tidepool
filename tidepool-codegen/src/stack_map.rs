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

#[derive(Debug, Clone)]
pub struct RawStackMapEntry {
    pub ty: cranelift_codegen::ir::types::Type,
    pub offset: u32,
}

#[derive(Debug, Clone)]
pub struct RawStackMap {
    pub code_offset: u32,
    pub frame_size: u32,
    pub entries: Vec<RawStackMapEntry>,
}

/// Maps absolute return addresses to stack map info.
///
/// Key = function_base_ptr + code_offset
/// (i.e., the return address, which is what the frame walker sees as caller_pc).
/// Cranelift's `code_offset` for user stack maps already points to the
/// instruction AFTER the call (the return point).
#[derive(Debug, Default)]
pub struct StackMapRegistry {
    entries: BTreeMap<usize, StackMapInfo>,
    /// Known JIT function address ranges `(start, end)`.
    ///
    /// INVARIANT: sorted by `start` AND pairwise disjoint (no two
    /// ranges overlap or touch) — maintained by `insert_disjoint_range` on
    /// every `register()` call, never by a one-off/lazy sort. This is what
    /// makes the `contains_address` binary search *correct*, not just fast:
    /// for disjoint sorted intervals, at most one interval can contain any
    /// given address, and it is always the one with the largest
    /// `start <= addr`. Without the disjointness half of the invariant (sorted
    /// order alone), a nested pair like `(0, 100)` and `(10, 20)` would let
    /// the binary search land on the narrower `(10, 20)` and wrongly report
    /// `contains_address(50) == false`.
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
    pub fn register(&mut self, base_ptr: usize, size: u32, raw_entries: &[RawStackMap]) {
        Self::insert_disjoint_range(&mut self.ranges, base_ptr, base_ptr + size as usize);

        for entry in raw_entries {
            let return_addr = base_ptr + entry.code_offset as usize;
            let offsets: Vec<u32> = entry.entries.iter().map(|e| e.offset).collect();
            self.entries.insert(
                return_addr,
                StackMapInfo {
                    frame_size: entry.frame_size,
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
    ///
    /// Binary search: sound only because `ranges` is sorted AND pairwise
    /// disjoint (see the invariant on that field), which makes the last range
    /// with `start <= addr` the only one that can contain `addr`.
    pub fn contains_address(&self, addr: usize) -> bool {
        let idx = self.ranges.partition_point(|&(start, _)| start <= addr);
        match idx.checked_sub(1) {
            Some(i) => {
                let (start, end) = self.ranges[i];
                addr >= start && addr < end
            }
            None => false,
        }
    }

    /// Insert `[start, end)` into `ranges`, merging with any range(s) it
    /// overlaps or touches so the sorted+disjoint invariant holds afterward.
    ///
    /// Real JIT code ranges never overlap, so this is normally a same-length
    /// splice; the merge exists so the invariant survives overlapping or
    /// nested input rather than silently breaking `contains_address`.
    fn insert_disjoint_range(ranges: &mut Vec<(usize, usize)>, start: usize, end: usize) {
        let mut merged_start = start;
        let mut merged_end = end;
        // First index whose range could possibly overlap or touch the new
        // one — anything before it ends strictly before `start`. Ranges are
        // sorted by start with monotonically non-decreasing ends (a
        // consequence of the disjointness invariant), so `end < start` is
        // monotonic across the vector and `partition_point` applies.
        let lo = ranges.partition_point(|&(_, e)| e < merged_start);
        let mut hi = lo;
        while hi < ranges.len() && ranges[hi].0 <= merged_end {
            merged_start = merged_start.min(ranges[hi].0);
            merged_end = merged_end.max(ranges[hi].1);
            hi += 1;
        }
        ranges.splice(lo..hi, std::iter::once((merged_start, merged_end)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stack_map_contains_address_boundaries() {
        let mut registry = StackMapRegistry::new();
        let start: usize = 0x1000;
        let size: u32 = 0x100;
        let end = start + size as usize;

        registry.register(start, size, &[]);

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

    #[test]
    fn test_contains_address_empty_registry() {
        let registry = StackMapRegistry::new();
        assert!(!registry.contains_address(0));
        assert!(!registry.contains_address(usize::MAX));
    }

    #[test]
    fn test_contains_address_below_and_above_all_ranges() {
        let mut registry = StackMapRegistry::new();
        registry.register(0x2000, 0x100, &[]); // [0x2000, 0x2100)
        registry.register(0x4000, 0x100, &[]); // [0x4000, 0x4100)

        assert!(!registry.contains_address(0x1000), "below all ranges");
        assert!(!registry.contains_address(0x5000), "above all ranges");
        assert!(registry.contains_address(0x2050), "inside first range");
        assert!(registry.contains_address(0x4050), "inside second range");
        assert!(
            !registry.contains_address(0x3000),
            "in the gap between ranges"
        );
    }

    #[test]
    fn test_contains_address_adjacent_ranges() {
        let mut registry = StackMapRegistry::new();
        registry.register(0x1000, 0x10, &[]); // [0x1000, 0x1010)
        registry.register(0x1010, 0x10, &[]); // [0x1010, 0x1020) — touches the first

        // The exclusive/inclusive boundary between two adjacent ranges must
        // still classify every address correctly regardless of whether the
        // registry merges touching ranges internally.
        assert!(
            registry.contains_address(0x100f),
            "last byte of first range"
        );
        assert!(
            registry.contains_address(0x1010),
            "first byte of second range"
        );
        assert!(
            registry.contains_address(0x101f),
            "last byte of second range"
        );
        assert!(
            !registry.contains_address(0x1020),
            "one past the merged end"
        );
        assert!(
            !registry.contains_address(0x0fff),
            "one before the merged start"
        );
    }

    #[test]
    fn test_contains_address_overlapping_ranges() {
        let mut registry = StackMapRegistry::new();
        registry.register(0x1000, 0x20, &[]); // [0x1000, 0x1020)
        registry.register(0x1010, 0x20, &[]); // [0x1010, 0x1030) — partial overlap

        assert!(registry.contains_address(0x1005), "only in the first range");
        assert!(registry.contains_address(0x1015), "in the overlap");
        assert!(
            registry.contains_address(0x1025),
            "only in the second range"
        );
        assert!(
            !registry.contains_address(0x1030),
            "exclusive end of the union"
        );
    }

    #[test]
    fn test_contains_address_nested_range_regression() {
        // The shape that breaks a naive "binary search by start, check only
        // that one range's end" implementation: a later-registered, narrower
        // range whose start falls INSIDE an earlier, wider range. Without
        // merging on insert, a query in the wide range but past the narrow
        // range's end would incorrectly land on the narrow range and report
        // `false`.
        let mut registry = StackMapRegistry::new();
        registry.register(0x1000, 0x100, &[]); // [0x1000, 0x1100)
        registry.register(0x1010, 0x10, &[]); // [0x1010, 0x1020) — nested inside the first

        assert!(
            registry.contains_address(0x1080),
            "address inside the wide range but past the nested range's end \
             must still be found"
        );
        assert!(
            registry.contains_address(0x1015),
            "inside the nested range too"
        );
        assert!(
            !registry.contains_address(0x1100),
            "exclusive end of the union"
        );
    }
}
