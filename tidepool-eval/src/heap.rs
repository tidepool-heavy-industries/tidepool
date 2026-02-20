use crate::env::Env;
use crate::value::{ThunkId, Value};
use tidepool_repr::CoreExpr;

/// State of a thunk in the thunk store.
#[derive(Debug, Clone)]
pub enum ThunkState {
    /// Not yet evaluated: captured env + expression
    Unevaluated(Env, CoreExpr),
    /// Currently being forced (cycle detection)
    BlackHole,
    /// Already evaluated to a value
    Evaluated(Value),
}

/// Heap trait: thunk allocation and forcing.
/// The interpreter uses VecHeap. Codegen uses ArenaHeap (from core-heap).
pub trait Heap {
    /// Allocate a new thunk. Returns its id.
    fn alloc(&mut self, env: Env, expr: CoreExpr) -> ThunkId;

    /// Read the current state of a thunk.
    fn read(&self, id: ThunkId) -> &ThunkState;

    /// Write a new state to a thunk (for force protocol and LetRec back-patching).
    fn write(&mut self, id: ThunkId, state: ThunkState);
}

/// Simple Vec-backed heap for the interpreter. No GC.
#[derive(Debug, Default)]
pub struct VecHeap {
    thunks: Vec<ThunkState>,
}

impl VecHeap {
    pub fn new() -> Self {
        Self { thunks: Vec::new() }
    }
}

impl Heap for VecHeap {
    fn alloc(&mut self, env: Env, expr: CoreExpr) -> ThunkId {
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
    use tidepool_repr::{CoreFrame, Literal, RecursiveTree, VarId};

    #[test]
    fn test_vecheap_ops() {
        let mut heap = VecHeap::new();
        let env = Env::new();
        let expr = RecursiveTree {
            nodes: vec![CoreFrame::Var(VarId(0))],
        };

        let id1 = heap.alloc(env.clone(), expr.clone());
        let id2 = heap.alloc(env.clone(), expr.clone());
        let id3 = heap.alloc(env.clone(), expr.clone());

        assert_eq!(id1.0, 0);
        assert_eq!(id2.0, 1);
        assert_eq!(id3.0, 2);

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
    fn test_thunk_state_machine() {
        let mut heap = VecHeap::new();
        let env = Env::new();
        let expr = RecursiveTree {
            nodes: vec![CoreFrame::Var(VarId(0))],
        };
        let id = heap.alloc(env, expr);

        // Unevaluated
        match heap.read(id) {
            ThunkState::Unevaluated(_, _) => (),
            _ => panic!("Expected Unevaluated"),
        }

        // Force started: Unevaluated -> BlackHole
        heap.write(id, ThunkState::BlackHole);
        match heap.read(id) {
            ThunkState::BlackHole => (),
            _ => panic!("Expected BlackHole"),
        }

        // Force complete: BlackHole -> Evaluated
        let val = Value::Lit(Literal::LitInt(42));
        heap.write(id, ThunkState::Evaluated(val));
        match heap.read(id) {
            ThunkState::Evaluated(_) => (),
            _ => panic!("Expected Evaluated"),
        }
    }
}
