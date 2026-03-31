use std::collections::VecDeque;
use tidepool_eval::heap::Heap;
use tidepool_eval::value::ThunkId;

/// Error during garbage collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GcError {
    #[error("ThunkId {:?} not in forwarding table (not reachable during trace)", .0)]
    ThunkNotReachable(ThunkId),
}

/// Maps old ThunkIds to new ThunkIds.
pub struct ForwardingTable {
    pub(crate) mapping: Vec<Option<ThunkId>>,
}

impl ForwardingTable {
    /// Look up the new ID for an old ID.
    /// Returns Err if the old ID was not reachable during trace.
    pub fn lookup(&self, old_id: ThunkId) -> Result<ThunkId, GcError> {
        let idx = old_id.0 as usize;
        if idx >= self.mapping.len() {
            return Err(GcError::ThunkNotReachable(old_id));
        }
        self.mapping[idx].ok_or(GcError::ThunkNotReachable(old_id))
    }

    /// Check if an old ID is reachable.
    pub fn is_reachable(&self, old_id: ThunkId) -> bool {
        let idx = old_id.0 as usize;
        idx < self.mapping.len() && self.mapping[idx].is_some()
    }
}

/// Trace reachable thunks starting from roots.
pub fn trace(roots: &[ThunkId], heap: &dyn Heap) -> ForwardingTable {
    let mut mapping: Vec<Option<ThunkId>> = Vec::new();
    let mut queue = VecDeque::new();
    let mut next_new_id = 0;

    for &root in roots {
        queue.push_back(root);
    }

    while let Some(old_id) = queue.pop_front() {
        let idx = old_id.0 as usize;

        if idx < mapping.len() && mapping[idx].is_some() {
            continue;
        }

        if idx >= mapping.len() {
            mapping.resize(idx + 1, None);
        }

        mapping[idx] = Some(ThunkId(next_new_id));
        next_new_id += 1;

        for child_id in heap.children_of(old_id) {
            queue.push_back(child_id);
        }
    }

    ForwardingTable { mapping }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_eval::env::Env;
    use tidepool_eval::heap::{ThunkState, VecHeap};
    use tidepool_eval::value::Value;
    use tidepool_repr::{CoreFrame, DataConId, Literal, RecursiveTree, VarId};

    fn empty_expr() -> tidepool_repr::CoreExpr {
        RecursiveTree {
            nodes: vec![CoreFrame::Var(VarId(0))],
        }
    }

    #[test]
    fn test_trace_empty_roots() {
        let heap = VecHeap::new();
        let table = trace(&[], &heap);
        assert!(table.mapping.is_empty());
    }

    #[test]
    fn test_trace_single_root() {
        let mut heap = VecHeap::new();
        let id = heap.alloc(Env::new(), empty_expr());
        let table = trace(&[id], &heap);
        assert_eq!(table.lookup(id).unwrap(), ThunkId(0));
    }

    #[test]
    fn test_trace_transitive_closure() {
        let mut heap = VecHeap::new();
        let id2 = heap.alloc(Env::new(), empty_expr());

        let mut env1 = Env::new();
        env1.insert(VarId(0), Value::ThunkRef(id2));
        let id1 = heap.alloc(env1, empty_expr());

        let table = trace(&[id1], &heap);
        assert_eq!(table.lookup(id1).unwrap(), ThunkId(0));
        assert_eq!(table.lookup(id2).unwrap(), ThunkId(1));
    }

    #[test]
    fn test_trace_double_referenced() {
        let mut heap = VecHeap::new();
        let id_shared = heap.alloc(Env::new(), empty_expr());

        let mut env1 = Env::new();
        env1.insert(VarId(0), Value::ThunkRef(id_shared));
        let id1 = heap.alloc(env1, empty_expr());

        let mut env2 = Env::new();
        env2.insert(VarId(0), Value::ThunkRef(id_shared));
        let id2 = heap.alloc(env2, empty_expr());

        let table = trace(&[id1, id2], &heap);
        assert_eq!(table.lookup(id1).unwrap(), ThunkId(0));
        assert_eq!(table.lookup(id2).unwrap(), ThunkId(1));
        assert_eq!(table.lookup(id_shared).unwrap(), ThunkId(2));

        // Ensure no other IDs are in the table (max 3 reachable)
        let reachable_count = table.mapping.iter().flatten().count();
        assert_eq!(reachable_count, 3);
    }

    #[test]
    fn test_trace_blackhole_reachable() {
        let mut heap = VecHeap::new();
        let id = heap.alloc(Env::new(), empty_expr());
        heap.write(id, ThunkState::BlackHole);

        let table = trace(&[id], &heap);
        assert_eq!(table.lookup(id).unwrap(), ThunkId(0));
    }

    #[test]
    fn test_trace_complex_value() {
        let mut heap = VecHeap::new();
        let id3 = heap.alloc(Env::new(), empty_expr());
        let id2 = heap.alloc(Env::new(), empty_expr());

        let val = Value::Con(
            DataConId(1),
            vec![
                Value::Lit(Literal::LitInt(1)),
                Value::ThunkRef(id2),
                Value::Con(DataConId(2), vec![Value::ThunkRef(id3)]),
            ],
        );

        let id1 = heap.alloc(Env::new(), empty_expr());
        heap.write(id1, ThunkState::Evaluated(val));

        let table = trace(&[id1], &heap);
        assert_eq!(table.lookup(id1).unwrap(), ThunkId(0));
        assert_eq!(table.lookup(id2).unwrap(), ThunkId(1));
        assert_eq!(table.lookup(id3).unwrap(), ThunkId(2));
    }
}
