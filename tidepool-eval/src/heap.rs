//! Thunk storage and lazy evaluation state.

use crate::env::Env;
use crate::value::{ThunkId, Value};
use tidepool_repr::CoreExpr;

/// The evaluation state of a lazy thunk.
///
/// Follows the standard GHC lifecycle: `Unevaluated` -> `BlackHole` -> `Evaluated`.
#[derive(Debug, Clone)]
pub enum ThunkState {
    /// Initial state: captured environment and expression to be evaluated.
    Unevaluated(Env, CoreExpr),
    /// Under evaluation: used to detect infinite loops (circular dependencies).
    BlackHole,
    /// Final state: has been successfully evaluated to WHNF.
    Evaluated(Value),
}

/// The outcome of [`Heap::begin_force`].
///
/// `Evaluating` is the only variant that changed the heap: `Unevaluated` was
/// just consumed into `BlackHole`, and the returned [`EvaluatingToken`] is
/// the ONLY way to leave that state again — `complete`/`restore` are its
/// sole consuming methods, so a caller cannot leave a thunk permanently
/// mid-force, complete a thunk that was never started, or (by holding two
/// tokens) overwrite one thunk's saved payload with another's.
pub enum ForceStart {
    /// Already `Evaluated`; here is the value.
    AlreadyEvaluated(Value),
    /// Already `BlackHole` — a circular dependency (`<<loop>>`).
    BlackHole,
    /// Was `Unevaluated`; now `BlackHole`.
    Evaluating(EvaluatingToken),
}

/// A thunk mid-force (see [`ForceStart::Evaluating`]): the heap slot is
/// `BlackHole` until this token is consumed by exactly one of
/// [`complete`](EvaluatingToken::complete)/[`restore`](EvaluatingToken::restore).
pub struct EvaluatingToken {
    id: ThunkId,
    env: Env,
    expr: CoreExpr,
}

impl EvaluatingToken {
    /// The thunk's captured environment, to evaluate `expr` under.
    pub fn env(&self) -> &Env {
        &self.env
    }

    /// The thunk's captured expression.
    pub fn expr(&self) -> &CoreExpr {
        &self.expr
    }

    /// Force succeeded: `BlackHole -> Evaluated(val)`. Returns `val` back
    /// unchanged, so the caller can chain straight into `Ok(token.complete(..))`.
    pub fn complete(self, heap: &mut dyn Heap, val: Value) -> Value {
        heap.finish_evaluated(self.id, val.clone());
        val
    }

    /// Force failed: restore the EXACT prior `Unevaluated(env, expr)`
    /// (rather than leaving `BlackHole` behind), so a later force retries
    /// instead of misreporting `InfiniteLoop` against a failure that has
    /// nothing to do with a real circular dependency.
    pub fn restore(self, heap: &mut dyn Heap) {
        heap.finish_unevaluated(self.id, self.env, self.expr);
    }
}

/// A `LetRec` knot-tying placeholder (see [`Heap::reserve`]): allocated
/// empty, to be filled exactly once — by `fill_evaluated` (a lambda RHS,
/// which captures without forcing) or `fill_unevaluated` (any other RHS) —
/// once every sibling binding in the group has its own placeholder id.
/// Consuming `self` on fill makes "reserved but never filled" a type-level
/// impossibility for any code path that holds onto the token.
pub struct ReservedThunk {
    id: ThunkId,
}

impl ReservedThunk {
    /// The reserved id, usable immediately — e.g. to build the recursive
    /// `Value::ThunkRef` binding — before the slot is filled.
    pub fn id(&self) -> ThunkId {
        self.id
    }

    /// Fill with an already-evaluated value (the lambda-RHS case).
    pub fn fill_evaluated(self, heap: &mut dyn Heap, val: Value) {
        heap.finish_evaluated(self.id, val);
    }

    /// Fill with an unevaluated `(env, expr)` pair, to be forced on demand.
    pub fn fill_unevaluated(self, heap: &mut dyn Heap, env: Env, expr: CoreExpr) {
        heap.finish_unevaluated(self.id, env, expr);
    }
}

/// Sealed: the raw state mutators are reachable only through
/// [`EvaluatingToken`]/[`ReservedThunk`]'s consuming methods, never as an
/// open `write`. A private supertrait of [`Heap`] keeps `finish_evaluated`/
/// `finish_unevaluated`/`finish_blackhole` unreachable from outside this
/// crate even though `Heap` itself is public — the standard sealed-trait
/// pattern; a private supertrait bound does not leak the trait.
pub(crate) trait HeapSeal {
    fn finish_evaluated(&mut self, id: ThunkId, val: Value);
    fn finish_unevaluated(&mut self, id: ThunkId, env: Env, expr: CoreExpr);
    fn finish_blackhole(&mut self, id: ThunkId);
}

/// Abstract storage for thunks.
///
/// Decouples the interpreter from the concrete memory management strategy,
/// allowing for simple vector-backed heaps or more complex garbage-collected
/// arenas.
// `HeapSeal` being `pub(crate)` is the point (see its doc comment): it keeps
// `finish_*` unreachable outside this crate. That also means an external
// crate could never implement `Heap` itself — fine, since only `VecHeap`
// (this crate) does — so the `private_bounds` lint is expected here, not a
// mistake.
#[allow(private_bounds)]
pub trait Heap: HeapSeal {
    /// Reserve an ID and store an unevaluated expression.
    fn alloc(&mut self, env: Env, expr: CoreExpr) -> ThunkId;

    /// Retrieve the current state of a thunk.
    fn read(&self, id: ThunkId) -> &ThunkState;

    /// Get all thunks directly referenced from this thunk's current state.
    /// Callers (e.g., GC) are responsible for performing any transitive traversal.
    fn children_of(&self, id: ThunkId) -> Vec<ThunkId>;

    /// Begin forcing `id`. See [`ForceStart`].
    fn begin_force(&mut self, id: ThunkId) -> ForceStart {
        match self.read(id).clone() {
            ThunkState::Evaluated(v) => ForceStart::AlreadyEvaluated(v),
            ThunkState::BlackHole => ForceStart::BlackHole,
            ThunkState::Unevaluated(env, expr) => {
                self.finish_blackhole(id);
                ForceStart::Evaluating(EvaluatingToken { id, env, expr })
            }
        }
    }

    /// Reserve an empty `LetRec` knot-tying placeholder. See [`ReservedThunk`].
    fn reserve(&mut self) -> ReservedThunk {
        let id = self.alloc(Env::new(), CoreExpr { nodes: vec![] });
        ReservedThunk { id }
    }
}

/// Simple Vec-backed heap for the interpreter. No GC.
#[derive(Debug, Default)]
pub struct VecHeap {
    thunks: Vec<ThunkState>,
}

impl VecHeap {
    /// Create a new, empty thunk store.
    pub fn new() -> Self {
        Self { thunks: Vec::new() }
    }

    fn collect_thunk_refs(val: &Value) -> Vec<ThunkId> {
        let mut refs = Vec::new();
        let mut stack = vec![val];
        while let Some(v) = stack.pop() {
            match v {
                Value::ThunkRef(id) => refs.push(*id),
                Value::Con(_, fields) => stack.extend(fields.iter().rev()),
                Value::ConFun(_, _, args) => stack.extend(args.iter().rev()),
                Value::Closure { env, .. } => stack.extend(env.values()),
                Value::JoinCont { env, .. } => stack.extend(env.values()),
                Value::Lit(_) => {}
                Value::ByteArray(_) => {}
            }
        }
        refs
    }
}

impl HeapSeal for VecHeap {
    fn finish_evaluated(&mut self, id: ThunkId, val: Value) {
        self.thunks[id.0 as usize] = ThunkState::Evaluated(val);
    }

    fn finish_unevaluated(&mut self, id: ThunkId, env: Env, expr: CoreExpr) {
        self.thunks[id.0 as usize] = ThunkState::Unevaluated(env, expr);
    }

    fn finish_blackhole(&mut self, id: ThunkId) {
        self.thunks[id.0 as usize] = ThunkState::BlackHole;
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

    fn children_of(&self, id: ThunkId) -> Vec<ThunkId> {
        match self.read(id) {
            ThunkState::Unevaluated(env, _) => {
                env.values().flat_map(Self::collect_thunk_refs).collect()
            }
            ThunkState::BlackHole => vec![],
            ThunkState::Evaluated(val) => Self::collect_thunk_refs(val),
        }
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

        let token = match heap.begin_force(id1) {
            ForceStart::Evaluating(t) => t,
            _ => panic!("Expected Evaluating"),
        };
        match heap.read(id1) {
            ThunkState::BlackHole => (),
            _ => panic!("Expected BlackHole"),
        }

        let val = Value::Lit(Literal::LitInt(100));
        token.complete(&mut heap, val);
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

        // Force started: Unevaluated -> BlackHole, captured in a token.
        let token = match heap.begin_force(id) {
            ForceStart::Evaluating(t) => t,
            _ => panic!("Expected Evaluating"),
        };
        match heap.read(id) {
            ThunkState::BlackHole => (),
            _ => panic!("Expected BlackHole"),
        }

        // Force complete: BlackHole -> Evaluated.
        let val = Value::Lit(Literal::LitInt(42));
        token.complete(&mut heap, val);
        match heap.read(id) {
            ThunkState::Evaluated(_) => (),
            _ => panic!("Expected Evaluated"),
        }
    }

    /// F2: a second `begin_force` on an already-`BlackHole` thunk must report
    /// `BlackHole` (the `<<loop>>` detection path), not silently hand out
    /// another token for the same slot.
    #[test]
    fn begin_force_on_blackhole_reports_blackhole_not_a_token() {
        let mut heap = VecHeap::new();
        let expr = RecursiveTree {
            nodes: vec![CoreFrame::Var(VarId(0))],
        };
        let id = heap.alloc(Env::new(), expr);

        let _token = match heap.begin_force(id) {
            ForceStart::Evaluating(t) => t,
            _ => panic!("Expected Evaluating"),
        };
        assert!(matches!(heap.begin_force(id), ForceStart::BlackHole));
    }

    /// F2: `EvaluatingToken::restore` must put back the EXACT prior
    /// `(env, expr)` pair, so a later force retries the same expression
    /// instead of tripping `InfiniteLoop` against a leftover `BlackHole`.
    #[test]
    fn restore_reverts_to_unevaluated_with_original_payload() {
        let mut heap = VecHeap::new();
        let expr = RecursiveTree {
            nodes: vec![CoreFrame::Var(VarId(7))],
        };
        let id = heap.alloc(Env::new(), expr);

        let token = match heap.begin_force(id) {
            ForceStart::Evaluating(t) => t,
            _ => panic!("Expected Evaluating"),
        };
        token.restore(&mut heap);

        match heap.read(id) {
            ThunkState::Unevaluated(_, e) => {
                assert!(matches!(e.nodes[0], CoreFrame::Var(VarId(7))));
            }
            other => panic!("Expected Unevaluated, got {other:?}"),
        }
    }

    /// F2: `reserve` + `fill_evaluated` must round-trip through the SAME id
    /// `reserve` handed back — the LetRec knot-tying protocol depends on
    /// binding that id into sibling RHSes before the fill happens.
    #[test]
    fn reserve_then_fill_evaluated_lands_on_the_reserved_id() {
        let mut heap = VecHeap::new();
        let reserved = heap.reserve();
        let id = reserved.id();
        match heap.read(id) {
            ThunkState::Unevaluated(_, expr) => assert!(expr.nodes.is_empty()),
            other => panic!("Expected empty Unevaluated placeholder, got {other:?}"),
        }

        reserved.fill_evaluated(&mut heap, Value::Lit(Literal::LitInt(9)));
        match heap.read(id) {
            ThunkState::Evaluated(Value::Lit(Literal::LitInt(9))) => (),
            other => panic!("Expected Evaluated(9), got {other:?}"),
        }
    }
}
