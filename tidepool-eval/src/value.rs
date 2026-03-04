use crate::env::Env;
use std::sync::{Arc, Mutex};
use tidepool_repr::{CoreExpr, DataConId, Literal, VarId};

/// Shared mutable byte array — `Arc<Mutex>` for in-place mutation semantics
/// and Send requirement (tidepool-mcp spawns threads).
/// IMPORTANT: Never hold two locks simultaneously on different SharedByteArrays
/// within a single primop — always clone data out first to avoid deadlock.
pub type SharedByteArray = Arc<Mutex<Vec<u8>>>;

/// Runtime value for the tree-walking interpreter.
#[derive(Debug, Clone)]
pub enum Value {
    /// Literal value (Int, Word, Char, String, Float, Double)
    Lit(Literal),
    /// Fully-applied data constructor
    Con(DataConId, Vec<Value>),
    /// Closure: captured env + binder + body
    Closure(Env, VarId, CoreExpr),
    /// Reference to a heap-allocated thunk
    ThunkRef(ThunkId),
    /// Join point continuation (lives in Env only, never heap-allocated)
    JoinCont(Vec<VarId>, CoreExpr, Env),
    /// Partially-applied data constructor: (tag, arity, accumulated args)
    /// When all args are supplied, collapses to Con.
    ConFun(DataConId, usize, Vec<Value>),
    /// Mutable/immutable byte array (ByteArray# / MutableByteArray#)
    ByteArray(SharedByteArray),
}

/// Thunk identifier — index into the thunk store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ThunkId(pub u32);

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
            Value::ByteArray(ba) => {
                let bytes = ba.lock().unwrap();
                write!(f, "<ByteArray# len={}>", bytes.len())
            }
        }
    }
}

impl Value {
    /// Count total nodes in a Value tree. O(n) walk used for size checks.
    pub fn node_count(&self) -> usize {
        match self {
            Value::Con(_, fields) => 1 + fields.iter().map(|f| f.node_count()).sum::<usize>(),
            _ => 1,
        }
    }
}

#[cfg(test)]
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
        assert_eq!(Value::Lit(Literal::from(1.23f64)).to_string(), "1.23");
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
