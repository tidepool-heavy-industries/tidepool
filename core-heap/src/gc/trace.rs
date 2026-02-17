use crate::arena::ArenaHeap;
use core_eval::ThunkId;
use std::collections::{HashMap, HashSet, VecDeque};

/// Maps old ThunkId -> new ThunkId for every reachable object.
#[derive(Debug, Default)]
pub struct ForwardingTable {
    pub old_to_new: HashMap<u32, u32>,
}

impl ForwardingTable {
    /// Check if a thunk is present in the forwarding table.
    pub fn contains(&self, old: ThunkId) -> bool {
        self.old_to_new.contains_key(&old.0)
    }
    
    /// Get the new ThunkId for a given old ThunkId.
    pub fn get(&self, old: ThunkId) -> Option<ThunkId> {
        self.old_to_new.get(&old.0).map(|&n| ThunkId(n))
    }
    
    /// Number of entries in the forwarding table.
    pub fn len(&self) -> usize {
        self.old_to_new.len()
    }
    
    /// Check if the forwarding table is empty.
    pub fn is_empty(&self) -> bool {
        self.old_to_new.is_empty()
    }
}

/// Trace from roots, building a forwarding table of all reachable objects.
pub fn trace(roots: &[ThunkId], heap: &ArenaHeap) -> ForwardingTable {
    let mut table = ForwardingTable::default();
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    let mut next_id: u32 = 0;
    
    // Seed with roots
    for &root in roots {
        if !visited.contains(&root.0) {
            visited.insert(root.0);
            queue.push_back(root);
        }
    }
    
    // BFS traversal
    while let Some(id) = queue.pop_front() {
        // Assign new compacted id
        table.old_to_new.insert(id.0, next_id);
        next_id += 1;
        
        // Find children (ThunkIds referenced by this thunk's state)
        for child in heap.children_of(id) {
            if !visited.contains(&child.0) {
                visited.insert(child.0);
                queue.push_back(child);
            }
        }
    }
    
    table
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_eval::{Heap, ThunkState, value::Value};
    use core_eval::env::Env;
    use core_repr::{CoreFrame, CoreExpr, Literal, VarId, DataConId};

    fn empty_expr() -> CoreExpr {
        CoreExpr { nodes: vec![CoreFrame::Lit(Literal::LitInt(0))] }
    }

    #[test]
    fn test_empty_roots() {
        let heap = ArenaHeap::new();
        let table = trace(&[], &heap);
        assert!(table.is_empty());
    }

    #[test]
    fn test_single_root_no_children() {
        let mut heap = ArenaHeap::new();
        let root = heap.alloc(Env::new(), empty_expr());
        let table = trace(&[root], &heap);
        assert_eq!(table.len(), 1);
        assert!(table.contains(root));
        assert_eq!(table.get(root), Some(ThunkId(0)));
    }

    #[test]
    fn test_root_with_children() {
        let mut heap = ArenaHeap::new();
        
        // Create child thunks
        let child1 = heap.alloc(Env::new(), empty_expr());
        let child2 = heap.alloc(Env::new(), empty_expr());
        
        // Create root referencing children
        let mut env = Env::new();
        env.insert(VarId(0), Value::ThunkRef(child1));
        env.insert(VarId(1), Value::ThunkRef(child2));
        let root = heap.alloc(env, empty_expr());
        
        let table = trace(&[root], &heap);
        assert_eq!(table.len(), 3);
        assert!(table.contains(root));
        assert!(table.contains(child1));
        assert!(table.contains(child2));
    }

    #[test]
    fn test_unreachable_not_in_table() {
        let mut heap = ArenaHeap::new();
        let root = heap.alloc(Env::new(), empty_expr());
        let unreachable = heap.alloc(Env::new(), empty_expr());
        
        let table = trace(&[root], &heap);
        assert_eq!(table.len(), 1);
        assert!(table.contains(root));
        assert!(!table.contains(unreachable));
    }

    #[test]
    fn test_diamond_pattern() {
        let mut heap = ArenaHeap::new();
        
        // D
        let d = heap.alloc(Env::new(), empty_expr());
        
        // B -> D
        let mut env_b = Env::new();
        env_b.insert(VarId(0), Value::ThunkRef(d));
        let b = heap.alloc(env_b, empty_expr());
        
        // C -> D
        let mut env_c = Env::new();
        env_c.insert(VarId(0), Value::ThunkRef(d));
        let c = heap.alloc(env_c, empty_expr());
        
        // A -> B, A -> C
        let mut env_a = Env::new();
        env_a.insert(VarId(0), Value::ThunkRef(b));
        env_a.insert(VarId(1), Value::ThunkRef(c));
        let a = heap.alloc(env_a, empty_expr());
        
        let table = trace(&[a], &heap);
        assert_eq!(table.len(), 4);
        assert!(table.contains(a));
        assert!(table.contains(b));
        assert!(table.contains(c));
        assert!(table.contains(d));
        
        // Check that D is only assigned one new ID (implicit in ForwardingTable structure)
    }

    #[test]
    fn test_blackhole_traced() {
        let mut heap = ArenaHeap::new();
        let root = heap.alloc(Env::new(), empty_expr());
        heap.write(root, ThunkState::BlackHole);
        
        let table = trace(&[root], &heap);
        assert_eq!(table.len(), 1);
        assert!(table.contains(root));
    }

    #[test]
    fn test_cycle() {
        let mut heap = ArenaHeap::new();
        
        // Allocate placeholders
        let a = heap.alloc(Env::new(), empty_expr());
        let b = heap.alloc(Env::new(), empty_expr());
        
        // A -> B
        let mut env_a = Env::new();
        env_a.insert(VarId(0), Value::ThunkRef(b));
        heap.write(a, ThunkState::Unevaluated(env_a, empty_expr()));
        
        // B -> A
        let mut env_b = Env::new();
        env_b.insert(VarId(0), Value::ThunkRef(a));
        heap.write(b, ThunkState::Unevaluated(env_b, empty_expr()));
        
        let table = trace(&[a], &heap);
        assert_eq!(table.len(), 2);
        assert!(table.contains(a));
        assert!(table.contains(b));
    }

    #[test]
    fn test_evaluated_con_refs() {
        let mut heap = ArenaHeap::new();
        
        let child = heap.alloc(Env::new(), empty_expr());
        let con_val = Value::Con(DataConId(1), vec![Value::ThunkRef(child)]);
        let root = heap.alloc(Env::new(), empty_expr());
        heap.write(root, ThunkState::Evaluated(con_val));
        
        let table = trace(&[root], &heap);
        assert_eq!(table.len(), 2);
        assert!(table.contains(root));
        assert!(table.contains(child));
    }

    #[test]
    fn test_evaluated_closure_refs() {
        let mut heap = ArenaHeap::new();
        
        let child = heap.alloc(Env::new(), empty_expr());
        let mut env = Env::new();
        env.insert(VarId(0), Value::ThunkRef(child));
        
        let closure_val = Value::Closure(env, VarId(1), empty_expr());
        let root = heap.alloc(Env::new(), empty_expr());
        heap.write(root, ThunkState::Evaluated(closure_val));
        
        let table = trace(&[root], &heap);
        assert_eq!(table.len(), 2);
        assert!(table.contains(root));
        assert!(table.contains(child));
    }

    #[test]
    fn test_evaluated_joincont_refs() {
        let mut heap = ArenaHeap::new();
        
        let child = heap.alloc(Env::new(), empty_expr());
        let mut env = Env::new();
        env.insert(VarId(0), Value::ThunkRef(child));
        
        let join_val = Value::JoinCont(vec![VarId(1)], empty_expr(), env);
        let root = heap.alloc(Env::new(), empty_expr());
        heap.write(root, ThunkState::Evaluated(join_val));
        
        let table = trace(&[root], &heap);
        assert_eq!(table.len(), 2);
        assert!(table.contains(root));
        assert!(table.contains(child));
    }
}
