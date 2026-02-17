use crate::types::{Alt, DataConId, JoinId, Literal, PrimOpKind, VarId};

/// A single node in the Core expression tree.
/// Parameterized over `A` to support both direct recursion and flat-vector indices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreFrame<A> {
    Var(VarId),
    Lit(Literal),
    App {
        fun: A,
        arg: A,
    },
    Lam {
        binder: VarId,
        body: A,
    },
    LetNonRec {
        binder: VarId,
        rhs: A,
        body: A,
    },
    LetRec {
        bindings: Vec<(VarId, A)>,
        body: A,
    },
    Case {
        scrutinee: A,
        binder: VarId,
        alts: Vec<Alt<A>>,
    },
    Con {
        tag: DataConId,
        fields: Vec<A>,
    },
    Join {
        label: JoinId,
        params: Vec<VarId>,
        rhs: A,
        body: A,
    },
    Jump {
        label: JoinId,
        args: Vec<A>,
    },
    PrimOp {
        op: PrimOpKind,
        args: Vec<A>,
    },
}
