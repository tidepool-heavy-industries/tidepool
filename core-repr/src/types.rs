/// Variable identifier. Wraps a numeric ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VarId(pub u64);

/// Join point label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct JoinId(pub u64);

/// Data constructor identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DataConId(pub u64);

/// Literal values. Matches GHC's post-O2 literal types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Literal {
    LitInt(i64),
    LitWord(u64),
    LitChar(char),
    LitString(Vec<u8>),
    LitFloat(u64),   // IEEE 754 bits
    LitDouble(u64),  // IEEE 754 bits
}

/// Primitive operations — the ~30-40 GHC.Prim operations we handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimOpKind {
    IntAdd, IntSub, IntMul,
    IntNegate,
    IntEq, IntNe, IntLt, IntLe, IntGt, IntGe,
    WordAdd, WordSub, WordMul,
    WordEq, WordNe, WordLt, WordLe, WordGt, WordGe,
    DoubleAdd, DoubleSub, DoubleMul, DoubleDiv,
    DoubleEq, DoubleNe, DoubleLt, DoubleLe, DoubleGt, DoubleGe,
    CharEq, CharNe, CharLt, CharLe, CharGt, CharGe,
    IndexArray,
    SeqOp,
    TagToEnum,
    DataToTag,
}

/// Case alternative constructor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AltCon {
    DataAlt(DataConId),
    LitAlt(Literal),
    Default,
}

/// A case alternative: constructor pattern + bound variables + body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alt<A> {
    pub con: AltCon,
    pub binders: Vec<VarId>,
    pub body: A,
}
