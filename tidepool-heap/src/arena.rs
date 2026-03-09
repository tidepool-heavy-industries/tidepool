use bumpalo::Bump;
use std::sync::atomic::{AtomicUsize, Ordering};
use thiserror::Error;
use tidepool_eval::value::Value;
use tidepool_eval::{Heap, ThunkId, ThunkState};
use tidepool_repr::CoreExpr;

#[derive(Debug, Error)]
pub enum HeapError {
    #[error("Nursery exhausted: requested {requested}, available {available}")]
    NurseryExhausted { requested: usize, available: usize },
}

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

const MAX_NURSERY_SIZE: usize = 512 * 1024 * 1024; // 512 MiB

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

    /// Check whether `size` bytes can be allocated without triggering GC.
    pub fn nursery_has_space(&self, size: usize) -> bool {
        let aligned_size = (size + 7) & !7;
        let used = self.used.load(Ordering::SeqCst);
        used.checked_add(aligned_size)
            .is_some_and(|total| total <= self.nursery_limit)
    }

    /// Allocate raw bytes in the arena.
    ///
    /// # Safety
    ///
    /// The returned pointer is 8-byte aligned and valid for `size` bytes.
    pub fn alloc_raw(&self, size: usize) -> Result<*mut u8, HeapError> {
        // Round up to 8-byte alignment, check for overflow.
        let aligned_size =
            size.checked_add(7)
                .map(|s| s & !7)
                .ok_or(HeapError::NurseryExhausted {
                    requested: size,
                    available: 0,
                })?;

        // Check for nursery exhaustion and reserve space atomically.
        let mut prev_used = self.used.load(Ordering::SeqCst);
        loop {
            if prev_used
                .checked_add(aligned_size)
                .is_none_or(|new_used| new_used > self.nursery_limit)
            {
                return Err(HeapError::NurseryExhausted {
                    requested: aligned_size,
                    available: self.nursery_limit - prev_used,
                });
            }
            match self.used.compare_exchange_weak(
                prev_used,
                prev_used + aligned_size,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(actual) => prev_used = actual,
            }
        }

        // bumpalo::alloc_layout always returns 16-byte aligned pointer
        // for layouts with alignment <= 16.
        let layout = std::alloc::Layout::from_size_align(aligned_size, 8).map_err(|_| {
            HeapError::NurseryExhausted {
                requested: aligned_size,
                available: 0,
            }
        })?;

        Ok(self.arena.alloc_layout(layout).as_ptr())
    }

    /// Bytes currently used in the nursery.
    pub fn bytes_used(&self) -> usize {
        self.used.load(Ordering::SeqCst)
    }

    /// Nursery capacity in bytes.
    pub fn nursery_limit(&self) -> usize {
        self.nursery_limit
    }

    /// Number of thunks currently allocated.
    pub fn thunk_count(&self) -> usize {
        self.thunks.len()
    }

    /// Run GC: trace from roots, compact live thunks, replace thunk store.
    /// Returns the forwarding table so callers can update their root references.
    pub fn collect_garbage(&mut self, roots: &[ThunkId]) -> crate::gc::trace::ForwardingTable {
        let (new_heap, table) = crate::gc::collect(roots, self);

        // Replace thunk store with compacted thunks via Heap trait
        let reachable_count = table.mapping.iter().flatten().count();
        self.thunks.clear();
        for i in 0..reachable_count {
            self.thunks.push(new_heap.read(ThunkId(i as u32)).clone());
        }

        // Reset raw arena (codegen path)
        self.arena.reset();
        self.used.store(0, Ordering::SeqCst);

        // Nursery doubling: if live thunks > 75% of pre-GC count, grow capacity
        self.grow_nursery_if_needed(reachable_count);

        table
    }

    fn grow_nursery_if_needed(&mut self, reachable_count: usize) {
        if reachable_count > 0 {
            let pre_gc_capacity = self.nursery_limit;
            let live_bytes = self.thunks.len() * std::mem::size_of::<ThunkState>();
            if live_bytes > pre_gc_capacity * 3 / 4 {
                if self.nursery_limit < MAX_NURSERY_SIZE {
                    self.nursery_limit = (self.nursery_limit * 2).min(MAX_NURSERY_SIZE);
                }
            }
        }
    }

    /// Return all ThunkIds directly referenced by this thunk.
    pub fn children_of(&self, id: ThunkId) -> Vec<ThunkId> {
        match self.read(id) {
            ThunkState::Unevaluated(env, _) => {
                env.values().flat_map(Self::collect_thunk_refs).collect()
            }
            ThunkState::BlackHole => vec![],
            ThunkState::Evaluated(val) => Self::collect_thunk_refs(val),
        }
    }

    fn collect_thunk_refs(val: &Value) -> Vec<ThunkId> {
        match val {
            Value::ThunkRef(id) => vec![*id],
            Value::Con(_, fields) => fields.iter().flat_map(Self::collect_thunk_refs).collect(),
            Value::ConFun(_, _, args) => args.iter().flat_map(Self::collect_thunk_refs).collect(),
            Value::Closure(env, _, _) => env.values().flat_map(Self::collect_thunk_refs).collect(),
            Value::JoinCont(_, _, env) => env.values().flat_map(Self::collect_thunk_refs).collect(),
            Value::Lit(_) => vec![],
            Value::ByteArray(_) => vec![],
        }
    }
}

impl Default for ArenaHeap {
    fn default() -> Self {
        Self::new()
    }
}

impl Heap for ArenaHeap {
    fn alloc(&mut self, env: tidepool_eval::env::Env, expr: CoreExpr) -> ThunkId {
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
    use crate::layout::*;
    use tidepool_eval::env::Env;
    use tidepool_eval::value::Value;
    use tidepool_repr::{CoreFrame, Literal, RecursiveTree, VarId};

    #[test]
    fn test_heap_trait_impl() {
        let mut heap = ArenaHeap::new();
        let env = Env::new();
        let expr = RecursiveTree {
            nodes: vec![CoreFrame::Var(VarId(0))],
        };

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
            let ptr = heap.alloc_raw(13).unwrap();
            assert_eq!(
                ptr as usize % 8,
                0,
                "Pointer {:?} is not 8-byte aligned",
                ptr
            );
        }
    }

    #[test]
    fn test_alloc_raw_roundtrip() {
        let heap = ArenaHeap::new();
        let size = 16;
        let ptr = heap.alloc_raw(size).unwrap();

        // SAFETY: ptr was returned by alloc_raw(16), so it is 8-byte aligned and valid for
        // 16 bytes -- sufficient for a HeapObject header. write_header/read_tag/read_size
        // operate within this allocation.
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

        heap.alloc_raw(128).unwrap();
        assert_eq!(heap.bytes_used(), 128);
    }

    #[test]
    fn test_nursery_exhaustion() {
        let heap = ArenaHeap::with_capacity(128);
        // Fill up to the limit (note: bumpalo might have small internal overhead per alloc but usually zero for bump-pointer)
        // With 8-byte alignment, 128 should be exactly possible.
        heap.alloc_raw(128).unwrap();
        // Next allocation should return error.
        assert!(heap.alloc_raw(8).is_err());
    }

    #[test]
    fn test_nursery_has_space() {
        let heap = ArenaHeap::with_capacity(128);
        assert!(heap.nursery_has_space(64));
        assert!(heap.nursery_has_space(128));
        assert!(!heap.nursery_has_space(129));
        heap.alloc_raw(64).unwrap();
        assert!(heap.nursery_has_space(64));
        assert!(!heap.nursery_has_space(65));
    }

    #[test]
    fn test_collect_garbage() {
        let mut heap = ArenaHeap::new();
        let env = Env::new();
        let expr = RecursiveTree {
            nodes: vec![CoreFrame::Var(VarId(0))],
        };

        // Allocate 3 thunks: id0, id1, id2
        let id0 = heap.alloc(env.clone(), expr.clone());
        let _id1 = heap.alloc(env.clone(), expr.clone()); // unreachable
        let id2 = heap.alloc(env.clone(), expr.clone());

        assert_eq!(heap.thunk_count(), 3);

        // GC with only id0 and id2 as roots
        let table = heap.collect_garbage(&[id0, id2]);

        // Only 2 live thunks remain
        assert_eq!(heap.thunk_count(), 2);

        // Forwarding table maps old -> new
        let new_id0 = table.lookup(id0).unwrap();
        let new_id2 = table.lookup(id2).unwrap();
        assert_eq!(new_id0, ThunkId(0));
        assert_eq!(new_id2, ThunkId(1));

        // Both are readable
        match heap.read(new_id0) {
            ThunkState::Unevaluated(_, _) => (),
            _ => panic!("Expected Unevaluated"),
        }
        match heap.read(new_id2) {
            ThunkState::Unevaluated(_, _) => (),
            _ => panic!("Expected Unevaluated"),
        }
    }

    #[test]
    fn test_collect_garbage_rewrites_refs() {
        let mut heap = ArenaHeap::new();
        let env = Env::new();
        let expr = RecursiveTree {
            nodes: vec![CoreFrame::Var(VarId(0))],
        };

        // id0 is a leaf thunk
        let id0 = heap.alloc(env.clone(), expr.clone());
        // id1 references id0 in its env
        let mut env_with_ref = Env::new();
        env_with_ref.insert(VarId(42), Value::ThunkRef(id0));
        let id1 = heap.alloc(env_with_ref, expr.clone());

        let table = heap.collect_garbage(&[id1]);

        assert_eq!(heap.thunk_count(), 2);
        let new_id1 = table.lookup(id1).unwrap();
        match heap.read(new_id1) {
            ThunkState::Unevaluated(env, _) => {
                // The ThunkRef should point to the NEW id for id0
                let new_id0 = table.lookup(id0).unwrap();
                match env.get(&VarId(42)).unwrap() {
                    Value::ThunkRef(id) => assert_eq!(*id, new_id0),
                    _ => panic!("Expected ThunkRef"),
                }
            }
            _ => panic!("Expected Unevaluated"),
        }
    }

    #[test]
    fn test_nursery_doubling() {
        let mut heap = ArenaHeap::with_capacity(1024 * 1024); // 1MB
        let env = Env::new();
        let expr = RecursiveTree {
            nodes: vec![CoreFrame::Var(VarId(0))],
        };
        // Allocate enough to trigger doubling (> 75% of 1MB)
        let thunk_size = std::mem::size_of::<ThunkState>();
        let count = (1024 * 1024 * 3 / 4 / thunk_size) + 1000;
        let mut roots = Vec::new();
        for _ in 0..count {
            roots.push(heap.alloc(env.clone(), expr.clone()));
        }

        heap.collect_garbage(&roots);
        // Should have doubled to 2MB
        assert_eq!(heap.nursery_limit(), 2 * 1024 * 1024);
    }

    #[test]
    fn test_nursery_cap() {
        let mut heap = ArenaHeap::with_capacity(MAX_NURSERY_SIZE - 1024);

        // Setup state to trigger doubling
        let thunk_size = std::mem::size_of::<ThunkState>();
        let count = (heap.nursery_limit * 3 / 4 / thunk_size) + 1000;
        heap.thunks.resize(
            count,
            ThunkState::Evaluated(Value::Lit(tidepool_repr::Literal::LitInt(0))),
        );

        // Trigger doubling directly via helper
        heap.grow_nursery_if_needed(count);

        // Should be capped at MAX_NURSERY_SIZE
        assert_eq!(heap.nursery_limit(), MAX_NURSERY_SIZE);
    }

    #[test]
    fn test_no_overlap() {
        let heap = ArenaHeap::new();
        let ptr1 = heap.alloc_raw(8).unwrap();
        let ptr2 = heap.alloc_raw(8).unwrap();

        assert_ne!(ptr1, ptr2);
        let diff = (ptr2 as usize).abs_diff(ptr1 as usize);
        assert!(diff >= 8);
    }
}
