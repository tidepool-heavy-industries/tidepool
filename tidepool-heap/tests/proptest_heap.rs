use proptest::prelude::*;
use tidepool_eval::{value::Value, Env, Heap, ThunkState};
use tidepool_heap::*;
use tidepool_repr::{CoreFrame, RecursiveTree, VarId};

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
        let ptr = heap.alloc_raw(aligned_size as usize).unwrap();

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
            let ptr = heap.alloc_raw(16).unwrap();
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
        let ptr = heap.alloc_raw(size).unwrap();
        assert_eq!(ptr as usize % 8, 0, "Pointer {:?} is not 8-byte aligned for size {}", ptr, size);
    }

    /// Test that an object created with arity N has its tag and size preserved.
    #[test]
    fn prop_tag_preserved(tag in any_tag()) {
        let heap = ArenaHeap::new();
        let size = 24;
        let ptr = heap.alloc_raw(size).unwrap();
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
        let ptr = heap.alloc_raw(size).unwrap();

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
            let new_id = table.lookup(id).unwrap();
            // Verify we can still read the thunk
            let ThunkState::Unevaluated(_, _) = heap.read(new_id) else {
                panic!("Expected Unevaluated thunk state after GC");
            };
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

        for &id in &ids[0..mid] {
            assert!(table.is_reachable(id));
        }
        for &id in &ids[mid..num_thunks] {
            assert!(!table.is_reachable(id));
        }
    }

    /// Test that a long chain of thunks survives GC if the head is reachable.
    #[test]
    fn prop_gc_preserves_chain(chain_len in 1..100usize) {
        let mut heap = ArenaHeap::new();
        let expr = RecursiveTree {
            nodes: vec![CoreFrame::Var(VarId(0))],
        };

        let mut prev_id = None;
        let mut ids = Vec::new();

        for i in 0..chain_len {
            let mut env = Env::new();
            if let Some(id) = prev_id {
                env.insert(VarId(i as u64), Value::ThunkRef(id));
            }
            let id = heap.alloc(env, expr.clone());
            ids.push(id);
            prev_id = Some(id);
        }

        // The head of the chain (last allocated) is our root
        let root = ids.last().cloned().unwrap();
        let table = heap.collect_garbage(&[root]);

        // All thunks in the chain should be reachable
        for &id in &ids {
            prop_assert!(table.is_reachable(id), "Thunk in chain should be reachable");
        }

        // Verify the chain structure is preserved
        let mut current_new_id = table.lookup(root).unwrap();
        for i in (1..chain_len).rev() {
            let ThunkState::Unevaluated(env, _) = heap.read(current_new_id) else {
                panic!("Expected Unevaluated thunk");
            };
            let prev_old_id = ids[i - 1];
            let expected_new_id = table.lookup(prev_old_id).unwrap();
            let Value::ThunkRef(id) = env.get(&VarId(i as u64)).expect("Value not found in env") else {
                panic!("Expected ThunkRef");
            };
            prop_assert_eq!(*id, expected_new_id, "Chain link broken at index {}", i);
            current_new_id = table.lookup(prev_old_id).unwrap();
        }
    }

    /// Test that objects surviving one GC also survive subsequent GCs if still reachable.
    #[test]
    fn prop_repeated_gc_stable(num_cycles in 2..5usize, objects_per_cycle in 5..20usize) {
        let mut heap = ArenaHeap::new();
        let expr = RecursiveTree {
            nodes: vec![CoreFrame::Var(VarId(0))],
        };

        let mut roots = Vec::new();

        for cycle in 0..num_cycles {
            // Allocate new objects in each cycle
            for i in 0..objects_per_cycle {
                let id = heap.alloc(Env::new(), expr.clone());
                // Only some of them become roots to be kept across cycles
                if i % 2 == 0 {
                    roots.push(id);
                }
            }

            // Run GC
            let table = heap.collect_garbage(&roots);

            // Update roots with their new IDs
            let mut new_roots = Vec::new();
            for &old_root in &roots {
                new_roots.push(table.lookup(old_root).unwrap());
            }
            roots = new_roots;

            // Verify all roots are valid
            for &root in &roots {
                let ThunkState::Unevaluated(_, _) = heap.read(root) else {
                    panic!("Expected Unevaluated thunk after cycle {}", cycle);
                };
            }

            prop_assert_eq!(heap.thunk_count(), roots.len(), "Heap should only contain roots");
        }
    }

    /// Test that GC reclaims space from unreachable objects.
    #[test]
    fn prop_gc_reclaims_proportionally(n_live in 10..50usize, n_dead in 10..50usize) {
        let mut heap = ArenaHeap::new();
        let expr = RecursiveTree {
            nodes: vec![CoreFrame::Var(VarId(0))],
        };

        let mut live_ids = Vec::new();
        for _ in 0..n_live {
            live_ids.push(heap.alloc(Env::new(), expr.clone()));
        }

        for _ in 0..n_dead {
            heap.alloc(Env::new(), expr.clone());
        }

        // Also allocate some raw memory in nursery
        let raw_ptr = heap.alloc_raw(1024).unwrap();
        unsafe { write_header(raw_ptr, TAG_LIT, 1024); }
        let used_before = heap.bytes_used();
        prop_assert!(used_before >= 1024);

        let pre_gc_thunk_count = heap.thunk_count();
        prop_assert_eq!(pre_gc_thunk_count, n_live + n_dead);

        // Run GC
        let _table = heap.collect_garbage(&live_ids);

        // Thunk count should be exactly n_live
        prop_assert_eq!(heap.thunk_count(), n_live);

        // Nursery should be reset
        prop_assert_eq!(heap.bytes_used(), 0);

        // We should be able to allocate at least as many as we had before in the nursery
        // (and more, since it's empty now)
        prop_assert!(heap.nursery_has_space(used_before));
        let new_raw_ptr = heap.alloc_raw(used_before).unwrap();
        prop_assert_eq!(heap.bytes_used(), used_before);
        unsafe { write_header(new_raw_ptr, TAG_LIT, used_before as u16); }
    }
}
