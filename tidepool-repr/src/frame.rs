//! Core IR frame definition.

use crate::types::{Alt, DataConId, JoinId, Literal, PrimOpKind, VarId};

/// A single node in the Core expression tree.
/// Parameterized over `A` to support both direct recursion and flat-vector indices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreFrame<A> {
    /// A variable reference.
    Var(VarId),
    /// A literal value.
    Lit(Literal),
    /// Function application.
    App {
        /// The function being applied.
        fun: A,
        /// The argument to the function.
        arg: A,
    },
    /// Lambda abstraction.
    Lam {
        /// The variable bound by this lambda.
        binder: VarId,
        /// The body of the lambda.
        body: A,
    },
    /// Non-recursive let binding.
    LetNonRec {
        /// The variable being bound.
        binder: VarId,
        /// The right-hand side of the binding.
        rhs: A,
        /// The body in which the binding is in scope.
        body: A,
    },
    /// Recursive let bindings.
    LetRec {
        /// The recursive bindings.
        bindings: Vec<(VarId, A)>,
        /// The body in which the bindings are in scope.
        body: A,
    },
    /// Case expression for pattern matching.
    Case {
        /// The expression being scrutinized.
        scrutinee: A,
        /// The binder for the scrutinee's value.
        binder: VarId,
        /// The case alternatives.
        alts: Vec<Alt<A>>,
    },
    /// Data constructor application.
    Con {
        /// The data constructor being applied.
        tag: DataConId,
        /// The fields of the constructor.
        fields: Vec<A>,
    },
    /// Join point definition.
    Join {
        /// The label for the join point.
        label: JoinId,
        /// The parameters of the join point.
        params: Vec<VarId>,
        /// The right-hand side of the join point.
        rhs: A,
        /// The body in which the join point is in scope.
        body: A,
    },
    /// Jump to a join point.
    Jump {
        /// The label of the join point to jump to.
        label: JoinId,
        /// The arguments passed to the join point.
        args: Vec<A>,
    },
    /// Primitive operation application.
    PrimOp {
        /// The primitive operation to apply.
        op: PrimOpKind,
        /// The arguments to the primitive operation.
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
