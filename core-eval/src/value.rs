use crate::env::Env;
use core_repr::{CoreExpr, DataConId, Literal, VarId};

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
}

/// Thunk identifier — index into the thunk store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ThunkId(pub u32);

#[cfg(test)]
mod tests {
    use super::*;
    use core_repr::{CoreFrame, RecursiveTree};

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
