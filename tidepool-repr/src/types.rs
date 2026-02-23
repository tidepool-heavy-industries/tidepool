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
    LitFloat(u64),  // IEEE 754 bits
    LitDouble(u64), // IEEE 754 bits
}

/// Primitive operations — the ~30-40 GHC.Prim operations we handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimOpKind {
    IntAdd,
    IntSub,
    IntMul,
    IntNegate,
    IntEq,
    IntNe,
    IntLt,
    IntLe,
    IntGt,
    IntGe,
    WordAdd,
    WordSub,
    WordMul,
    WordEq,
    WordNe,
    WordLt,
    WordLe,
    WordGt,
    WordGe,
    DoubleAdd,
    DoubleSub,
    DoubleMul,
    DoubleDiv,
    DoubleEq,
    DoubleNe,
    DoubleLt,
    DoubleLe,
    DoubleGt,
    DoubleGe,
    CharEq,
    CharNe,
    CharLt,
    CharLe,
    CharGt,
    CharGe,
    IndexArray,
    SeqOp,
    TagToEnum,
    DataToTag,
    IntQuot,
    IntRem,
    Chr,
    Ord,
    // --- Tier 2: Int bitwise ---
    IntAnd,
    IntOr,
    IntXor,
    IntNot,      // unary
    IntShl,      // uncheckedIShiftL#
    IntShra,     // uncheckedIShiftRA# (arithmetic right shift)
    IntShrl,     // uncheckedIShiftRL# (logical right shift)
    // --- Tier 2: Word arithmetic + bitwise ---
    WordQuot,
    WordRem,
    WordAnd,
    WordOr,
    WordXor,
    WordNot,     // unary
    WordShl,     // uncheckedShiftL#
    WordShrl,    // uncheckedShiftRL#
    // --- Tier 2: Int↔Word conversions ---
    Int2Word,
    Word2Int,
    // --- Tier 2: Narrowing ---
    Narrow8Int,
    Narrow16Int,
    Narrow32Int,
    Narrow8Word,
    Narrow16Word,
    Narrow32Word,
    // --- Tier 2: Float arithmetic + comparison ---
    FloatAdd,
    FloatSub,
    FloatMul,
    FloatDiv,
    FloatNegate,  // unary
    FloatEq,
    FloatNe,
    FloatLt,
    FloatLe,
    FloatGt,
    FloatGe,
    // --- Tier 2: Double extras ---
    DoubleNegate, // unary
    // --- Tier 2: Type conversions ---
    Int2Double,
    Double2Int,
    Int2Float,
    Float2Int,
    Double2Float,
    Float2Double,
    // --- Tier 3: Pointer equality (polyfill: always 0 = not equal) ---
    ReallyUnsafePtrEquality,
    // --- Tier 3: Addr# ---
    IndexCharOffAddr,
    PlusAddr,
    // --- Tier 3: Exception ---
    Raise,
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

impl std::fmt::Display for VarId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "v_{}", self.0)
    }
}

impl std::fmt::Display for JoinId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "j_{}", self.0)
    }
}

impl std::fmt::Display for DataConId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Con_{}", self.0)
    }
}

impl std::fmt::Display for Literal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Literal::LitInt(n) => write!(f, "{}#", n),
            Literal::LitWord(n) => write!(f, "{}##", n),
            Literal::LitChar(c) => write!(f, "'{}'#", c),
            Literal::LitString(bs) => match std::str::from_utf8(bs) {
                Ok(s) => write!(f, "\"{}\"#", s),
                Err(_) => write!(f, "<bytes len={}>", bs.len()),
            },
            Literal::LitFloat(bits) => write!(f, "{}#", f32::from_bits(*bits as u32)),
            Literal::LitDouble(bits) => write!(f, "{}##", f64::from_bits(*bits)),
        }
    }
}

impl std::fmt::Display for PrimOpKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            PrimOpKind::IntAdd => "+#",
            PrimOpKind::IntSub => "-#",
            PrimOpKind::IntMul => "*#",
            PrimOpKind::IntNegate => "negateInt#",
            PrimOpKind::IntEq => "==#",
            PrimOpKind::IntNe => "/=#",
            PrimOpKind::IntLt => "<#",
            PrimOpKind::IntLe => "<=#",
            PrimOpKind::IntGt => ">#",
            PrimOpKind::IntGe => ">=#",
            PrimOpKind::WordAdd => "plusWord#",
            PrimOpKind::WordSub => "minusWord#",
            PrimOpKind::WordMul => "timesWord#",
            PrimOpKind::WordEq => "eqWord#",
            PrimOpKind::WordNe => "neWord#",
            PrimOpKind::WordLt => "ltWord#",
            PrimOpKind::WordLe => "leWord#",
            PrimOpKind::WordGt => "gtWord#",
            PrimOpKind::WordGe => "geWord#",
            PrimOpKind::DoubleAdd => "+##",
            PrimOpKind::DoubleSub => "-##",
            PrimOpKind::DoubleMul => "*##",
            PrimOpKind::DoubleDiv => "/##",
            PrimOpKind::DoubleEq => "==##",
            PrimOpKind::DoubleNe => "/=##",
            PrimOpKind::DoubleLt => "<##",
            PrimOpKind::DoubleLe => "<=##",
            PrimOpKind::DoubleGt => ">##",
            PrimOpKind::DoubleGe => ">=##",
            PrimOpKind::CharEq => "eqChar#",
            PrimOpKind::CharNe => "neChar#",
            PrimOpKind::CharLt => "ltChar#",
            PrimOpKind::CharLe => "leChar#",
            PrimOpKind::CharGt => "gtChar#",
            PrimOpKind::CharGe => "geChar#",
            PrimOpKind::IndexArray => "indexArray#",
            PrimOpKind::SeqOp => "seq",
            PrimOpKind::TagToEnum => "tagToEnum#",
            PrimOpKind::DataToTag => "dataToTag#",
            PrimOpKind::IntQuot => "quotInt#",
            PrimOpKind::IntRem => "remInt#",
            PrimOpKind::Chr => "chr#",
            PrimOpKind::Ord => "ord#",
            PrimOpKind::IntAnd => "andI#",
            PrimOpKind::IntOr => "orI#",
            PrimOpKind::IntXor => "xorI#",
            PrimOpKind::IntNot => "notI#",
            PrimOpKind::IntShl => "uncheckedIShiftL#",
            PrimOpKind::IntShra => "uncheckedIShiftRA#",
            PrimOpKind::IntShrl => "uncheckedIShiftRL#",
            PrimOpKind::WordQuot => "quotWord#",
            PrimOpKind::WordRem => "remWord#",
            PrimOpKind::WordAnd => "and#",
            PrimOpKind::WordOr => "or#",
            PrimOpKind::WordXor => "xor#",
            PrimOpKind::WordNot => "not#",
            PrimOpKind::WordShl => "uncheckedShiftL#",
            PrimOpKind::WordShrl => "uncheckedShiftRL#",
            PrimOpKind::Int2Word => "int2Word#",
            PrimOpKind::Word2Int => "word2Int#",
            PrimOpKind::Narrow8Int => "narrow8Int#",
            PrimOpKind::Narrow16Int => "narrow16Int#",
            PrimOpKind::Narrow32Int => "narrow32Int#",
            PrimOpKind::Narrow8Word => "narrow8Word#",
            PrimOpKind::Narrow16Word => "narrow16Word#",
            PrimOpKind::Narrow32Word => "narrow32Word#",
            PrimOpKind::FloatAdd => "plusFloat#",
            PrimOpKind::FloatSub => "minusFloat#",
            PrimOpKind::FloatMul => "timesFloat#",
            PrimOpKind::FloatDiv => "divideFloat#",
            PrimOpKind::FloatNegate => "negateFloat#",
            PrimOpKind::FloatEq => "eqFloat#",
            PrimOpKind::FloatNe => "neFloat#",
            PrimOpKind::FloatLt => "ltFloat#",
            PrimOpKind::FloatLe => "leFloat#",
            PrimOpKind::FloatGt => "gtFloat#",
            PrimOpKind::FloatGe => "geFloat#",
            PrimOpKind::DoubleNegate => "negateDouble#",
            PrimOpKind::Int2Double => "int2Double#",
            PrimOpKind::Double2Int => "double2Int#",
            PrimOpKind::Int2Float => "int2Float#",
            PrimOpKind::Float2Int => "float2Int#",
            PrimOpKind::Double2Float => "double2Float#",
            PrimOpKind::Float2Double => "float2Double#",
            PrimOpKind::ReallyUnsafePtrEquality => "reallyUnsafePtrEquality#",
            PrimOpKind::IndexCharOffAddr => "indexCharOffAddr#",
            PrimOpKind::PlusAddr => "plusAddr#",
            PrimOpKind::Raise => "raise#",
        };
        f.write_str(name)
    }
}

impl std::fmt::Display for AltCon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AltCon::DataAlt(id) => write!(f, "{}", id),
            AltCon::LitAlt(lit) => write!(f, "{}", lit),
            AltCon::Default => write!(f, "_"),
        }
    }
}

impl From<i64> for Literal {
    fn from(n: i64) -> Self {
        Literal::LitInt(n)
    }
}

impl From<u64> for Literal {
    fn from(n: u64) -> Self {
        Literal::LitWord(n)
    }
}

impl From<char> for Literal {
    fn from(c: char) -> Self {
        Literal::LitChar(c)
    }
}

impl From<f64> for Literal {
    fn from(f: f64) -> Self {
        Literal::LitDouble(f.to_bits())
    }
}

impl From<f32> for Literal {
    fn from(f: f32) -> Self {
        Literal::LitFloat(f.to_bits() as u64)
    }
}

impl From<Vec<u8>> for Literal {
    fn from(bs: Vec<u8>) -> Self {
        Literal::LitString(bs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::TreeBuilder;
    use crate::frame::CoreFrame;

    #[test]
    fn test_var_id_display() {
        assert_eq!(VarId(42).to_string(), "v_42");
    }

    #[test]
    fn test_join_id_display() {
        assert_eq!(JoinId(7).to_string(), "j_7");
    }

    #[test]
    fn test_datacon_id_display() {
        assert_eq!(DataConId(3).to_string(), "Con_3");
    }

    #[test]
    fn test_literal_display() {
        assert_eq!(Literal::LitInt(42).to_string(), "42#");
        assert_eq!(Literal::LitWord(100).to_string(), "100##");
        assert_eq!(Literal::LitChar('x').to_string(), "'x'#");
        assert_eq!(
            Literal::LitString(b"hello".to_vec()).to_string(),
            "\"hello\"#"
        );
    }

    #[test]
    fn test_literal_from() {
        assert_eq!(Literal::from(42i64), Literal::LitInt(42));
        assert_eq!(Literal::from(100u64), Literal::LitWord(100));
        assert_eq!(Literal::from('x'), Literal::LitChar('x'));
        let d = Literal::from(3.14f64);
        assert!(matches!(d, Literal::LitDouble(_)));
        let f = Literal::from(2.5f32);
        assert!(matches!(f, Literal::LitFloat(_)));
    }

    #[test]
    fn test_primop_display() {
        assert_eq!(PrimOpKind::IntAdd.to_string(), "+#");
        assert_eq!(PrimOpKind::DoubleDiv.to_string(), "/##");
        assert_eq!(PrimOpKind::SeqOp.to_string(), "seq");
    }

    #[test]
    fn test_altcon_display() {
        assert_eq!(AltCon::Default.to_string(), "_");
        assert_eq!(AltCon::DataAlt(DataConId(5)).to_string(), "Con_5");
        assert_eq!(AltCon::LitAlt(Literal::LitInt(42)).to_string(), "42#");
    }

    #[test]
    fn test_tree_builder() {
        let mut b = TreeBuilder::new();
        let x = b.push(CoreFrame::Var(VarId(1)));
        let lit = b.push(CoreFrame::Lit(Literal::LitInt(42)));
        let _app = b.push(CoreFrame::App { fun: x, arg: lit });
        let expr = b.build();
        assert_eq!(expr.nodes.len(), 3);
    }
}
