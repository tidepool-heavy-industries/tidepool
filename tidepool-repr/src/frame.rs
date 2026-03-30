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

impl<A: std::fmt::Display> std::fmt::Display for CoreFrame<A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CoreFrame::Var(id) => write!(f, "Var({})", id),
            CoreFrame::Lit(lit) => write!(f, "Lit({})", lit),
            CoreFrame::App { fun, arg } => write!(f, "App({}, {})", fun, arg),
            CoreFrame::Lam { binder, body } => write!(f, "Lam({}, {})", binder, body),
            CoreFrame::LetNonRec { binder, rhs, body } => {
                write!(f, "LetNonRec({}, {}, {})", binder, rhs, body)
            }
            CoreFrame::LetRec { bindings, body } => {
                write!(f, "LetRec([")?;
                for (i, (b, r)) in bindings.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "({}, {})", b, r)?;
                }
                write!(f, "], {})", body)
            }
            CoreFrame::Case {
                scrutinee,
                binder,
                alts,
            } => write!(f, "Case({}, {}, {} alts)", scrutinee, binder, alts.len()),
            CoreFrame::Con { tag, fields } => {
                write!(f, "Con({}, [", tag)?;
                for (i, field) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", field)?;
                }
                write!(f, "])")
            }
            CoreFrame::Join {
                label,
                params,
                rhs,
                body,
            } => write!(
                f,
                "Join({}, {} params, {}, {})",
                label,
                params.len(),
                rhs,
                body
            ),
            CoreFrame::Jump { label, args } => {
                write!(f, "Jump({}, [", label)?;
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", a)?;
                }
                write!(f, "])")
            }
            CoreFrame::PrimOp { op, args } => {
                write!(f, "PrimOp({}, [", op)?;
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", a)?;
                }
                write!(f, "])")
            }
        }
    }
}
