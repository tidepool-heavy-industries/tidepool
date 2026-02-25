use proptest::prelude::*;
use tidepool_heap::*;
use tidepool_eval::{Env, Heap, ThunkState};
use tidepool_repr::{RecursiveTree, CoreFrame, VarId};

// Strategies for generating heap object data
fn any_tag() -> impl Strategy<Value = u8> {
    prop_oneof![
        Just(TAG_CLOSURE),
        Just(TAG_THUNK),
        Just(TAG_CON),
        Just(TAG_LIT),
    ]
}

proptest! {
    /// Test that we can allocate a raw object, write its header, and read it back correctly.
    #[test]
    fn prop_allocate_and_read_back(tag in any_tag(), size in 16..512u16) {
        let heap = ArenaHeap::new();
        // Ensure size is 8-byte aligned as required by alloc_raw and layout
        let aligned_size = (size + 7) & !7;
        let ptr = heap.alloc_raw(aligned_size as usize);

        unsafe {
            write_header(ptr, tag, aligned_size);
            assert_eq!(read_tag(ptr), tag);
            assert_eq!(read_size(ptr), aligned_size);
        }
    }

    /// Test that multiple allocations return distinct pointers that do not overlap.
    #[test]
    fn prop_multiple_allocations_dont_overlap(n in 2..50usize) {
        let heap = ArenaHeap::with_capacity(1024 * 1024);
        let mut ptrs = Vec::with_capacity(n);

        for _ in 0..n {
            let ptr = heap.alloc_raw(16);
            ptrs.push(ptr as usize);
        }

        let mut sorted_ptrs = ptrs.clone();
        sorted_ptrs.sort();

        for i in 0..n-1 {
            assert!(sorted_ptrs[i] + 16 <= sorted_ptrs[i+1], "Overlapping or insufficiently spaced pointers at index {}", i);
        }
    }

    /// Test that all allocated pointers are 8-byte aligned.
    /// This verifies the ArenaHeap's alignment guarantee regardless of requested size.
    #[test]
    fn prop_allocation_respects_alignment(size in 1..256usize) {
        let heap = ArenaHeap::new();
        let ptr = heap.alloc_raw(size);
        assert_eq!(ptr as usize % 8, 0, "Pointer {:?} is not 8-byte aligned for size {}", ptr, size);
    }

    /// Test that an object created with arity N has its tag and size preserved.
    #[test]
    fn prop_tag_preserved(tag in any_tag()) {
        let heap = ArenaHeap::new();
        let size = 24;
        let ptr = heap.alloc_raw(size);
        unsafe {
            write_header(ptr, tag, size as u16);
            assert_eq!(read_tag(ptr), tag);
        }
    }

    /// Test that a Con object with N fields respects the size layout.
    #[test]
    fn prop_field_count_matches_arity(num_fields in 0..20u16) {
        let heap = ArenaHeap::new();
        let size = CON_FIELDS_OFFSET + (num_fields as usize * FIELD_STRIDE);
        let ptr = heap.alloc_raw(size);

        unsafe {
            write_header(ptr, TAG_CON, size as u16);
            std::ptr::write_unaligned(ptr.add(CON_NUM_FIELDS_OFFSET) as *mut u16, num_fields);

            assert_eq!(read_tag(ptr), TAG_CON);
            assert_eq!(read_size(ptr), size as u16);
            let read_num_fields = std::ptr::read_unaligned(ptr.add(CON_NUM_FIELDS_OFFSET) as *const u16);
            assert_eq!(read_num_fields, num_fields);
        }
    }
}

// Property tests for garbage collection: verify that reachable thunks survive GC while unreachable thunks
// are collected, and that the forwarding table correctly maps old to new ThunkIds.
proptest! {
    /// Test that live thunks survive GC and their IDs are correctly updated.
    #[test]
    fn prop_live_objects_survive_gc(num_thunks in 10..50usize, root_indices in prop::collection::vec(0..50usize, 1..10)) {
        let mut heap = ArenaHeap::new();
        let env = Env::new();
        let expr = RecursiveTree {
            nodes: vec![CoreFrame::Var(VarId(0))],
        };

        let mut ids = Vec::new();
        for _ in 0..num_thunks {
            ids.push(heap.alloc(env.clone(), expr.clone()));
        }
        prop_assert!(!ids.is_empty(), "Test requires at least one thunk");

        // Filter root indices to be within bounds
        let mut roots = Vec::new();
        for &idx in &root_indices {
            if idx < ids.len() {
                roots.push(ids[idx]);
            }
        }

        if roots.is_empty() {
            roots.push(ids[0]);
        }

        let table = heap.collect_garbage(&roots);

        for &id in &roots {
            assert!(table.is_reachable(id));
            let new_id = table.lookup(id);
            // Verify we can still read the thunk
            match heap.read(new_id) {
                ThunkState::Unevaluated(_, _) => (),
                _ => panic!("Expected Unevaluated thunk state after GC"),
            }
        }
    }

    /// Test that unreachable thunks are collected (not in forwarding table).
    #[test]
    fn prop_unreachable_objects_are_collected(num_thunks in 10..20usize) {
        let mut heap = ArenaHeap::new();
        let env = Env::new();
        let expr = RecursiveTree {
            nodes: vec![CoreFrame::Var(VarId(0))],
        };

        let mut ids = Vec::new();
        for _ in 0..num_thunks {
            ids.push(heap.alloc(env.clone(), expr.clone()));
        }
        prop_assert!(!ids.is_empty(), "Test requires at least one thunk");

        // Only first half are roots
        let mid = num_thunks / 2;
        let roots = &ids[0..mid];

        let table = heap.collect_garbage(roots);

        for i in 0..mid {
            assert!(table.is_reachable(ids[i]));
        }
        for i in mid..num_thunks {
            assert!(!table.is_reachable(ids[i]));
        }
    }
}