/// Unique identifier for a variable binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VarId(pub u32);

/// Unique identifier for a join point label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct JoinId(pub u32);

/// Unique identifier for a data constructor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DataConId(pub u32);

/// Index into a RecursiveTree's node array.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(pub u32);

/// Literal values from GHC Core.
#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    LitInt(i64),
    LitWord(u64),
    LitFloat(f64),
    LitDouble(f64),
    LitChar(char),
    LitString(Vec<u8>),
}

/// Primitive operation kinds corresponding to GHC.Prim operations.
/// The Rust evaluator implements these as native hardware operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrimOpKind {
    // Integer arithmetic
    IntAdd,
    IntSub,
    IntMul,
    IntQuot,
    IntRem,
    IntNegate,
    // Integer comparison
    IntEq,
    IntNe,
    IntLt,
    IntLe,
    IntGt,
    IntGe,
    // Word arithmetic
    WordAdd,
    WordSub,
    WordMul,
    WordQuot,
    WordRem,
    // Double arithmetic
    DoubleAdd,
    DoubleSub,
    DoubleMul,
    DoubleDiv,
    DoubleNegate,
    // Double comparison
    DoubleEq,
    DoubleLt,
    DoubleLe,
    // Conversions
    Int2Double,
    Double2Int,
    Int2Word,
    Word2Int,
    // String operations
    UnpackCString,
    // Tag/enum operations
    TagToEnum,
    DataToTag,
    // Evaluation control
    Seq,
}

/// Case alternative constructor pattern.
#[derive(Debug, Clone, PartialEq)]
pub enum AltCon {
    /// Match a specific data constructor.
    DataAlt(DataConId),
    /// Match a specific literal value.
    LitAlt(Literal),
    /// Default/wildcard match.
    Default,
}

/// A case alternative: pattern, bound variables, and body expression.
#[derive(Debug, Clone, PartialEq)]
pub struct Alt<A> {
    pub con: AltCon,
    pub binders: Vec<VarId>,
    pub body: A,
}