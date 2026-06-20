//! Runtime values for the tree-walking interpreter.

use crate::env::Env;
use std::cell::RefCell;
use std::sync::{Arc, Mutex};
use tidepool_repr::{CoreExpr, DataConId, Literal, VarId};

/// Shared mutable byte array — `Arc<Mutex>` for in-place mutation semantics
/// and Send requirement (tidepool-mcp spawns threads).
/// IMPORTANT: Never hold two locks simultaneously on different SharedByteArrays
/// within a single primop — always clone data out first to avoid deadlock.
pub type SharedByteArray = Arc<Mutex<Vec<u8>>>;

/// Runtime value for the tree-walking interpreter.
///
/// Represents an object in Weak Head Normal Form (WHNF). This includes
/// fully-applied constructors, closures, literals, and references to
/// lazy thunks.
#[derive(Debug, Clone)]
pub enum Value {
    /// Primitive literal value (Int#, Word#, Char#, String#, Float#, Double#).
    Lit(Literal),
    /// Fully-applied data constructor (GHC DataCon).
    Con(DataConId, Vec<Value>),
    /// Function closure: captured env + binder + body (GHC PAP/FUN).
    Closure(Env, VarId, CoreExpr),
    /// Reference to a heap-allocated thunk (GHC Thunk).
    ThunkRef(ThunkId),
    /// Join point continuation (GHC Join Point).
    JoinCont(Vec<VarId>, CoreExpr, Env),
    /// Partially-applied data constructor function: carries the constructor id,
    /// its total arity, and the arguments applied so far. When the number of
    /// arguments reaches the arity, this is reduced to a `Con` value.
    ConFun(DataConId, usize, Vec<Value>),
    /// Mutable/immutable byte array (ByteArray# / MutableByteArray#).
    ByteArray(SharedByteArray),
}

/// Thunk identifier — index into the thunk store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ThunkId(
    /// Raw index into the heap's thunk vector.
    pub u32,
);

impl std::fmt::Display for ThunkId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<thunk#{}>", self.0)
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Lit(lit) => match lit {
                Literal::LitInt(n) => write!(f, "{}", n),
                Literal::LitWord(n) => write!(f, "{}", n),
                Literal::LitChar(c) => write!(f, "'{}'", c.escape_default()),
                Literal::LitString(bs) => match std::str::from_utf8(bs) {
                    Ok(s) => write!(f, "{:?}", s),
                    Err(_) => write!(f, "<bytes len={}>", bs.len()),
                },
                Literal::LitFloat(bits) => match u32::try_from(*bits) {
                    Ok(bits32) => write!(f, "{}", f32::from_bits(bits32)),
                    Err(_) => write!(f, "<invalid f32 bits=0x{:016x}>", *bits),
                },
                Literal::LitDouble(bits) => write!(f, "{}", f64::from_bits(*bits)),
            },
            Value::Con(id, fields) => {
                write!(f, "<Con#{}>", id.0)?;
                for field in fields {
                    write!(f, " {}", field)?;
                }
                Ok(())
            }
            Value::Closure(..) => write!(f, "<closure>"),
            Value::ThunkRef(id) => write!(f, "{}", id),
            Value::JoinCont(..) => write!(f, "<join>"),
            Value::ConFun(id, arity, args) => {
                write!(f, "<partial Con#{} {}/{}>", id.0, args.len(), arity)
            }
            Value::ByteArray(ba) => match ba.lock() {
                Ok(bytes) => write!(f, "<ByteArray# len={}>", bytes.len()),
                Err(_) => write!(f, "<ByteArray# poisoned>"),
            },
        }
    }
}

impl Value {
    /// Count total nodes in a Value tree. O(n) walk used for size checks.
    pub fn node_count(&self) -> usize {
        // Iterative: responses can be cons-spines tens of thousands deep;
        // per-cell recursion overflows small (test/tokio) thread stacks.
        let mut count = 0usize;
        let mut stack: Vec<&Value> = vec![self];
        while let Some(v) = stack.pop() {
            count += 1;
            if let Value::Con(_, fields) = v {
                stack.extend(fields.iter());
            }
        }
        count
    }
}

/// Work item for the iterative destructor: a sub-`Value` whose own children
/// still need detaching, or a captured environment that must drop without
/// re-entering this destructor along the call stack.
enum DropWork {
    Val(Value),
    Env(Env),
}

// The per-item cost of the explicit work list is a visible, testable property
// — the whole point of replacing the compiler's hidden recursive drop.
const _: () = assert!(std::mem::size_of::<DropWork>() <= 80);

thread_local! {
    /// Re-entrancy queue for the iterative `Value` destructor. `None` when no
    /// drop loop is running on this thread; `Some(queue)` while one is. A
    /// nested `Value::drop` (e.g. `im` dropping an env value) sees `Some`,
    /// hands its children to the running loop, and returns — turning spine
    /// depth into queue length.
    static DROP_QUEUE: RefCell<Option<Vec<DropWork>>> = const { RefCell::new(None) };
}

/// Detach the directly-owned recursive children of `v` into `queue`, leaving
/// `v` shallow so the compiler's field-drop glue does no recursive work.
///
/// `Con`/`ConFun` fields move into the queue as values. `Closure`/`JoinCont`
/// environments move in as whole `Env`s rather than being drained here:
/// draining would force `im`'s copy-on-write (`PoolRef::make_mut`) to deep-
/// CLONE every structurally shared binding — the interpreter captures envs by
/// `clone`, so sharing is the norm — re-introducing the very stack overflow
/// (and an O(env) perf cliff on every closure drop) we are removing. Each
/// `Env` is instead dropped later through `im`'s own refcount-aware
/// destructor: uniquely-owned values re-enter `Value::drop` (caught by the
/// running loop), shared values are merely decremented, never cloned. `im`'s
/// node recursion is bounded by HAMT depth, never the spine depth.
fn detach_children(v: &mut Value, queue: &mut Vec<DropWork>) {
    match v {
        Value::Con(_, fields) | Value::ConFun(_, _, fields) => {
            queue.extend(std::mem::take(fields).into_iter().map(DropWork::Val));
        }
        Value::Closure(env, _, _) | Value::JoinCont(_, _, env) => {
            if !env.is_empty() {
                queue.push(DropWork::Env(std::mem::take(env)));
            }
        }
        Value::Lit(_) | Value::ThunkRef(_) | Value::ByteArray(_) => {}
    }
}

/// Iterative destructor: the auto-derived recursive drop costs ~3 stack frames
/// per nested `Con` level, and effect responses / results can be cons-spines
/// (or CPS closure chains) tens of thousands deep — overflowing the host
/// thread's stack (a SIGSEGV outside JIT signal protection, which presents as
/// a silent thread death). Recursive children are detached onto a thread-local
/// work list before they drop, so the compiler-generated field drop only ever
/// sees empty `Vec`s / empty `Env`s.
///
/// Covers both spine families: `Con`/`ConFun` field spines AND
/// `Closure`/`JoinCont` environment chains (the latter via `im`'s own
/// refcount-aware drop — see [`detach_children`]).
impl Drop for Value {
    fn drop(&mut self) {
        // Leaves (and already-emptied containers) own nothing recursive — skip
        // the machinery entirely. This is the overwhelmingly common drop and
        // never touches the thread-local.
        match self {
            Value::Con(_, f) | Value::ConFun(_, _, f) if f.is_empty() => return,
            Value::Lit(_) | Value::ThunkRef(_) | Value::ByteArray(_) => return,
            _ => {}
        }

        DROP_QUEUE.with(|cell| {
            // Re-entrant: a drop loop is already running on this thread and we
            // are being dropped from inside it. Hand our children to that loop;
            // our field-drop glue then runs shallow.
            if let Some(queue) = cell.borrow_mut().as_mut() {
                detach_children(self, queue);
                return;
            }

            // We own the loop. Seed the queue with our children, then drain.
            let mut queue: Vec<DropWork> = Vec::new();
            detach_children(self, &mut queue);
            *cell.borrow_mut() = Some(queue);

            loop {
                // Release the borrow BEFORE dropping: the drop re-enters and
                // borrows again to push more work.
                let item = cell.borrow_mut().as_mut().and_then(Vec::pop);
                match item {
                    // `v`'s own `Value::drop` re-enters, detaches its children,
                    // and drops shallow.
                    Some(DropWork::Val(v)) => drop(v),
                    // `im`'s refcount-aware drop; uniquely-owned values re-enter
                    // `Value::drop` (caught above), shared values are merely
                    // decremented — never cloned.
                    Some(DropWork::Env(env)) => drop(env),
                    None => break,
                }
            }

            *cell.borrow_mut() = None;
        });
    }
}

#[cfg(test)]
#[allow(clippy::approx_constant)] // tests literal float formatting, not math constants
mod tests {
    use super::*;
    use tidepool_repr::{CoreFrame, RecursiveTree};

    #[test]
    fn test_value_display() {
        let env = Env::new();
        let expr = RecursiveTree {
            nodes: vec![CoreFrame::Var(VarId(0))],
        };

        assert_eq!(Value::Lit(Literal::LitInt(42)).to_string(), "42");
        assert_eq!(Value::Lit(Literal::LitChar('x')).to_string(), "'x'");
        assert_eq!(Value::Lit(Literal::LitChar('\n')).to_string(), r"'\n'");
        assert_eq!(
            Value::Lit(Literal::LitString(b"hello".to_vec())).to_string(),
            "\"hello\""
        );
        assert_eq!(
            Value::Lit(Literal::LitString(b"with \"quotes\"".to_vec())).to_string(),
            "\"with \\\"quotes\\\"\""
        );
        assert_eq!(Value::Lit(Literal::from(3.14f64)).to_string(), "3.14");
        assert_eq!(
            Value::Lit(Literal::LitFloat(0xFFFF_FFFF_FFFF_FFFF)).to_string(),
            "<invalid f32 bits=0xffffffffffffffff>"
        );

        assert_eq!(Value::Con(DataConId(1), vec![]).to_string(), "<Con#1>");
        assert_eq!(
            Value::Con(DataConId(1), vec![Value::Lit(Literal::LitInt(42))]).to_string(),
            "<Con#1> 42"
        );
        assert_eq!(
            Value::Con(
                DataConId(1),
                vec![
                    Value::Lit(Literal::LitInt(42)),
                    Value::Lit(Literal::LitString(b"hi".to_vec()))
                ]
            )
            .to_string(),
            "<Con#1> 42 \"hi\""
        );

        assert_eq!(
            Value::Closure(env.clone(), VarId(0), expr.clone()).to_string(),
            "<closure>"
        );
        assert_eq!(Value::ThunkRef(ThunkId(123)).to_string(), "<thunk#123>");
        assert_eq!(
            Value::JoinCont(vec![VarId(1)], expr, env).to_string(),
            "<join>"
        );

        assert_eq!(
            Value::ConFun(DataConId(1), 2, vec![Value::Lit(Literal::LitInt(42))]).to_string(),
            "<partial Con#1 1/2>"
        );
    }

    #[test]
    fn test_value_construction() {
        let env = Env::new();
        let lit = Value::Lit(Literal::LitInt(42));
        let con = Value::Con(DataConId(1), vec![lit.clone()]);

        let expr = RecursiveTree {
            nodes: vec![CoreFrame::Var(VarId(0))],
        };
        let closure = Value::Closure(env.clone(), VarId(0), expr.clone());
        let thunk = Value::ThunkRef(ThunkId(0));
        let join = Value::JoinCont(vec![VarId(1)], expr, env);

        match lit {
            Value::Lit(_) => (),
            _ => panic!("Expected Lit"),
        }
        match con {
            Value::Con(_, _) => (),
            _ => panic!("Expected Con"),
        }
        match closure {
            Value::Closure(_, _, _) => (),
            _ => panic!("Expected Closure"),
        }
        match thunk {
            Value::ThunkRef(_) => (),
            _ => panic!("Expected ThunkRef"),
        }
        match join {
            Value::JoinCont(_, _, _) => (),
            _ => panic!("Expected JoinCont"),
        }
    }

    #[test]
    fn test_closure_clone() {
        let env = Env::new();
        let expr = RecursiveTree {
            nodes: vec![CoreFrame::Var(VarId(0))],
        };
        let closure = Value::Closure(env, VarId(0), expr);
        let _cloned = closure.clone();
    }
}
