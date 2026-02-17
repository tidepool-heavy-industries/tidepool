use crate::types::*;

/// One layer of a GHC Core expression tree.
///
/// `A` represents child expression positions. When stored in a
/// `RecursiveTree`, `A = NodeId`. For recursion scheme intermediates,
/// `A` may be other types.
#[derive(Debug, Clone, PartialEq)]
pub enum CoreFrame<A> {
    /// Variable reference.
    Var(VarId),
    /// Literal value.
    Lit(Literal),
    /// Function application.
    App { fun: A, arg: A },
    /// Lambda abstraction.
    Lam { binder: VarId, body: A },
    /// Non-recursive let binding.
    LetNonRec { binder: VarId, rhs: A, body: A },
    /// Recursive let bindings (mutually recursive group).
    LetRec { bindings: Vec<(VarId, A)>, body: A },
    /// Case expression with scrutinee, case binder, and alternatives.
    Case { scrutinee: A, binder: VarId, alts: Vec<Alt<A>> },
    /// Saturated data constructor application.
    Con { tag: DataConId, fields: Vec<A> },
    /// Join point definition.
    Join { label: JoinId, params: Vec<VarId>, rhs: A, body: A },
    /// Jump to a join point.
    Jump { label: JoinId, args: Vec<A> },
    /// Saturated primitive operation.
    PrimOp { op: PrimOpKind, args: Vec<A> },
}

/// Functor mapping over the child positions of a frame.
///
/// This is the base functor interface that enables recursion schemes
/// (catamorphism, anamorphism, hylomorphism) over Core expressions.
pub trait MapLayer {
    /// The type of child references in this frame.
    type Child;
    /// The same frame structure but with a different child type.
    type Mapped<B>;

    /// Apply a function to every child position in the frame,
    /// preserving the frame's structure.
    fn map_layer<B>(self, f: impl FnMut(Self::Child) -> B) -> Self::Mapped<B>;
}

impl<A> MapLayer for CoreFrame<A> {
    type Child = A;
    type Mapped<B> = CoreFrame<B>;

    fn map_layer<B>(self, mut f: impl FnMut(A) -> B) -> CoreFrame<B> {
        match self {
            CoreFrame::Var(v) => CoreFrame::Var(v),
            CoreFrame::Lit(l) => CoreFrame::Lit(l),
            CoreFrame::App { fun, arg } => CoreFrame::App {
                fun: f(fun),
                arg: f(arg),
            },
            CoreFrame::Lam { binder, body } => CoreFrame::Lam {
                binder,
                body: f(body),
            },
            CoreFrame::LetNonRec { binder, rhs, body } => CoreFrame::LetNonRec {
                binder,
                rhs: f(rhs),
                body: f(body),
            },
            CoreFrame::LetRec { bindings, body } => CoreFrame::LetRec {
                bindings: bindings.into_iter().map(|(v, a)| (v, f(a))).collect(),
                body: f(body),
            },
            CoreFrame::Case { scrutinee, binder, alts } => CoreFrame::Case {
                scrutinee: f(scrutinee),
                binder,
                alts: alts
                    .into_iter()
                    .map(|alt| Alt {
                        con: alt.con,
                        binders: alt.binders,
                        body: f(alt.body),
                    })
                    .collect(),
            },
            CoreFrame::Con { tag, fields } => CoreFrame::Con {
                tag,
                fields: fields.into_iter().map(&mut f).collect(),
            },
            CoreFrame::Join { label, params, rhs, body } => CoreFrame::Join {
                label,
                params,
                rhs: f(rhs),
                body: f(body),
            },
            CoreFrame::Jump { label, args } => CoreFrame::Jump {
                label,
                args: args.into_iter().map(&mut f).collect(),
            },
            CoreFrame::PrimOp { op, args } => CoreFrame::PrimOp {
                op,
                args: args.into_iter().map(&mut f).collect(),
            },
        }
    }
}