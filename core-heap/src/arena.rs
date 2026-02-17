use bumpalo::Bump;
use core_eval::{Heap, ThunkState, ThunkId};
use core_repr::CoreExpr;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Arena-based heap for Tidepool.
///
/// For v1, the ArenaHeap manages thunks via Vec for the interpreter
/// and provides a bumpalo-backed arena for raw HeapObject allocation
/// which will be used by the codegen path.
pub struct ArenaHeap {
    /// Nursery for raw HeapObject allocation.
    arena: Bump,
    /// Thunk store: index from ThunkId -> ThunkState.
    thunks: Vec<ThunkState>,
    /// Capacity of the nursery in bytes.
    nursery_limit: usize,
    /// Bytes used for raw objects.
    used: AtomicUsize,
}

impl ArenaHeap {
    /// Create a new ArenaHeap with 4MB default nursery capacity.
    pub fn new() -> Self {
        Self::with_capacity(4 * 1024 * 1024) // 4MB default
    }

    /// Create a new ArenaHeap with specified nursery capacity in bytes.
    pub fn with_capacity(bytes: usize) -> Self {
        Self {
            arena: Bump::with_capacity(bytes),
            thunks: Vec::new(),
            nursery_limit: bytes,
            used: AtomicUsize::new(0),
        }
    }

    /// Allocate raw bytes in the arena.
    ///
    /// # Panics
    ///
    /// If the allocation exceeds the nursery limit.
    ///
    /// # Safety
    ///
    /// The returned pointer is 8-byte aligned and valid for `size` bytes.
    pub fn alloc_raw(&self, size: usize) -> *mut u8 {
        // Round up to 8-byte alignment.
        let aligned_size = (size + 7) & !7;
        
        // Check for nursery exhaustion.
        let old_used = self.used.fetch_add(aligned_size, Ordering::SeqCst);
        if old_used + aligned_size > self.nursery_limit {
            // v1 behavior: panic. Later we signal GC.
            panic!("Nursery limit exceeded: GC not yet implemented");
        }

        // bumpalo::alloc_layout always returns 16-byte aligned pointer
        // for layouts with alignment <= 16.
        let layout = std::alloc::Layout::from_size_align(aligned_size, 8)
            .expect("Invalid layout for alloc_raw");
        
        self.arena.alloc_layout(layout).as_ptr()
    }

    /// Bytes currently used in the nursery.
    pub fn bytes_used(&self) -> usize {
        self.used.load(Ordering::SeqCst)
    }

    /// Nursery capacity in bytes.
    pub fn nursery_limit(&self) -> usize {
        self.nursery_limit
    }
}

impl Default for ArenaHeap {
    fn default() -> Self {
        Self::new()
    }
}

impl Heap for ArenaHeap {
    fn alloc(&mut self, env: core_eval::env::Env, expr: CoreExpr) -> ThunkId {
        let id = ThunkId(self.thunks.len() as u32);
        self.thunks.push(ThunkState::Unevaluated(env, expr));
        id
    }

    fn read(&self, id: ThunkId) -> &ThunkState {
        &self.thunks[id.0 as usize]
    }

    fn write(&mut self, id: ThunkId, state: ThunkState) {
        self.thunks[id.0 as usize] = state;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_repr::{CoreFrame, RecursiveTree, Literal, VarId};
    use core_eval::env::Env;
    use core_eval::value::Value;
    use crate::layout::*;

    #[test]
    fn test_heap_trait_impl() {
        let mut heap = ArenaHeap::new();
        let env = Env::new();
        let expr = RecursiveTree { nodes: vec![CoreFrame::Var(VarId(0))] };

        let id1 = heap.alloc(env.clone(), expr.clone());
        let id2 = heap.alloc(env.clone(), expr.clone());

        assert_eq!(id1.0, 0);
        assert_eq!(id2.0, 1);

        match heap.read(id1) {
            ThunkState::Unevaluated(_, _) => (),
            _ => panic!("Expected Unevaluated"),
        }

        heap.write(id1, ThunkState::BlackHole);
        match heap.read(id1) {
            ThunkState::BlackHole => (),
            _ => panic!("Expected BlackHole"),
        }

        let val = Value::Lit(Literal::LitInt(100));
        heap.write(id1, ThunkState::Evaluated(val));
        match heap.read(id1) {
            ThunkState::Evaluated(Value::Lit(Literal::LitInt(100))) => (),
            _ => panic!("Expected Evaluated(100)"),
        }
    }

    #[test]
    fn test_alloc_raw_alignment() {
        let heap = ArenaHeap::new();
        
        for _ in 0..10 {
            let ptr = heap.alloc_raw(13);
            assert_eq!(ptr as usize % 8, 0, "Pointer {:?} is not 8-byte aligned", ptr);
        }
    }

    #[test]
    fn test_alloc_raw_roundtrip() {
        let heap = ArenaHeap::new();
        let size = 16;
        let ptr = heap.alloc_raw(size);

        unsafe {
            write_header(ptr, TAG_CLOSURE, size as u16);
            assert_eq!(read_tag(ptr), TAG_CLOSURE);
            assert_eq!(read_size(ptr), size as u16);
        }
    }

    #[test]
    fn test_nursery_capacity() {
        let heap = ArenaHeap::with_capacity(1024);
        assert_eq!(heap.nursery_limit(), 1024);
        assert_eq!(heap.bytes_used(), 0);

        heap.alloc_raw(128);
        assert_eq!(heap.bytes_used(), 128);
    }

    #[test]
    #[should_panic(expected = "Nursery limit exceeded")]
    fn test_nursery_exhaustion() {
        let heap = ArenaHeap::with_capacity(128);
        // Fill up to the limit (note: bumpalo might have small internal overhead per alloc but usually zero for bump-pointer)
        // With 8-byte alignment, 128 should be exactly possible.
        heap.alloc_raw(128);
        // Next allocation should panic.
        heap.alloc_raw(8);
    }

    #[test]
    fn test_no_overlap() {
        let heap = ArenaHeap::new();
        let ptr1 = heap.alloc_raw(8);
        let ptr2 = heap.alloc_raw(8);
        
        assert_ne!(ptr1, ptr2);
        let diff = (ptr2 as usize).abs_diff(ptr1 as usize);
        assert!(diff >= 8);
    }
}
