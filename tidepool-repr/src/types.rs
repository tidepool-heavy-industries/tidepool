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
    IntNot,  // unary
    IntShl,  // uncheckedIShiftL#
    IntShra, // uncheckedIShiftRA# (arithmetic right shift)
    IntShrl, // uncheckedIShiftRL# (logical right shift)
    // --- Tier 2: Word arithmetic + bitwise ---
    WordQuot,
    WordRem,
    WordAnd,
    WordOr,
    WordXor,
    WordNot,  // unary
    WordShl,  // uncheckedShiftL#
    WordShrl, // uncheckedShiftRL#
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
    FloatNegate, // unary
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
    // --- Tier 4: ByteArray# / MutableByteArray# ---
    NewByteArray,
    ReadWord8Array,
    WriteWord8Array,
    SizeofMutableByteArray,
    UnsafeFreezeByteArray,
    CopyByteArray,
    CopyMutableByteArray,
    CopyAddrToByteArray,
    ShrinkMutableByteArray,
    ResizeMutableByteArray,
    Clz8,
    IntToInt64,
    Int64ToWord64,
    TimesInt2Hi,
    TimesInt2Lo,
    TimesInt2Overflow,
    IndexWord8Array,
    IndexWord8OffAddr,
    CompareByteArrays,
    WordToWord8,
    Word64And,
    Int64ToInt,
    Word64ToInt64,
    Word8ToWord,
    Word8Lt,
    Int64Ge,
    Int64Negate,
    Int64Shra,
    Word64Shl,
    Word8Ge,
    Word8Sub,
    SizeofByteArray,
    IndexWordArray,
    Int64Add,
    Int64Gt,
    Int64Mul,
    Int64Lt,
    Int64Le,
    Int64Sub,
    Int64Shl,
    WriteWordArray,
    ReadWordArray,
    SetByteArray,
    Word64Or,
    Word8Add,
    Word8Le,
    AddIntCVal,
    AddIntCCarry,
    SubWordCVal,
    SubWordCCarry,
    AddWordCVal,
    AddWordCCarry,
    TimesWord2Hi,
    TimesWord2Lo,
    QuotRemWordVal,
    QuotRemWordRem,
    // --- Tier 5: FFI intrinsics (Data.Text C helpers) ---
    FfiStrlen,
    FfiTextMeasureOff,
    FfiTextMemchr,
    FfiTextReverse,
    // --- Tier 6: SmallArray# (boxed, no card table — used by HashMap) ---
    NewSmallArray,
    ReadSmallArray,
    WriteSmallArray,
    IndexSmallArray,
    SizeofSmallArray,
    SizeofSmallMutableArray,
    UnsafeFreezeSmallArray,
    UnsafeThawSmallArray,
    CopySmallArray,
    CopySmallMutableArray,
    CloneSmallArray,
    CloneSmallMutableArray,
    ShrinkSmallMutableArray,
    // --- Tier 6: Array# (boxed, with card table — used by Vector) ---
    NewArray,
    ReadArray,
    WriteArray,
    SizeofArray,
    SizeofMutableArray,
    UnsafeFreezeArray,
    UnsafeThawArray,
    CopyArray,
    CopyMutableArray,
    CloneArray,
    CloneMutableArray,
    // --- Tier 6: Bit operations ---
    PopCnt,
    PopCnt8,
    PopCnt16,
    PopCnt32,
    PopCnt64,
    Ctz,
    Ctz8,
    Ctz16,
    Ctz32,
    Ctz64,
    CasSmallArray,
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
            PrimOpKind::NewByteArray => "newByteArray#",
            PrimOpKind::ReadWord8Array => "readWord8Array#",
            PrimOpKind::WriteWord8Array => "writeWord8Array#",
            PrimOpKind::SizeofMutableByteArray => "sizeofMutableByteArray#",
            PrimOpKind::UnsafeFreezeByteArray => "unsafeFreezeByteArray#",
            PrimOpKind::CopyByteArray => "copyByteArray#",
            PrimOpKind::CopyMutableByteArray => "copyMutableByteArray#",
            PrimOpKind::CopyAddrToByteArray => "copyAddrToByteArray#",
            PrimOpKind::ShrinkMutableByteArray => "shrinkMutableByteArray#",
            PrimOpKind::ResizeMutableByteArray => "resizeMutableByteArray#",
            PrimOpKind::Clz8 => "clz8#",
            PrimOpKind::IntToInt64 => "intToInt64#",
            PrimOpKind::Int64ToWord64 => "int64ToWord64#",
            PrimOpKind::TimesInt2Hi => "timesInt2Hi",
            PrimOpKind::TimesInt2Lo => "timesInt2Lo",
            PrimOpKind::TimesInt2Overflow => "timesInt2Overflow",
            PrimOpKind::IndexWord8Array => "indexWord8Array#",
            PrimOpKind::IndexWord8OffAddr => "indexWord8OffAddr#",
            PrimOpKind::CompareByteArrays => "compareByteArrays#",
            PrimOpKind::WordToWord8 => "wordToWord8#",
            PrimOpKind::Word64And => "andWord64#",
            PrimOpKind::Int64ToInt => "int64ToInt#",
            PrimOpKind::Word64ToInt64 => "word64ToInt64#",
            PrimOpKind::Word8ToWord => "word8ToWord#",
            PrimOpKind::Word8Lt => "ltWord8#",
            PrimOpKind::Int64Ge => "geInt64#",
            PrimOpKind::Int64Negate => "negateInt64#",
            PrimOpKind::Int64Shra => "uncheckedIShiftRA64#",
            PrimOpKind::Word64Shl => "uncheckedShiftL64#",
            PrimOpKind::Word8Ge => "geWord8#",
            PrimOpKind::Word8Sub => "subWord8#",
            PrimOpKind::SizeofByteArray => "sizeofByteArray#",
            PrimOpKind::IndexWordArray => "indexWordArray#",
            PrimOpKind::Int64Add => "plusInt64#",
            PrimOpKind::Int64Gt => "gtInt64#",
            PrimOpKind::Int64Lt => "ltInt64#",
            PrimOpKind::Int64Le => "leInt64#",
            PrimOpKind::Int64Sub => "subInt64#",
            PrimOpKind::Int64Mul => "timesInt64#",
            PrimOpKind::Int64Shl => "uncheckedIShiftL64#",
            PrimOpKind::WriteWordArray => "writeWordArray#",
            PrimOpKind::ReadWordArray => "readWordArray#",
            PrimOpKind::SetByteArray => "setByteArray#",
            PrimOpKind::Word64Or => "or64#",
            PrimOpKind::Word8Add => "plusWord8#",
            PrimOpKind::Word8Le => "leWord8#",
            PrimOpKind::AddIntCVal => "addIntC#_val",
            PrimOpKind::AddIntCCarry => "addIntC#_overflow",
            PrimOpKind::SubWordCVal => "subWordC#_val",
            PrimOpKind::SubWordCCarry => "subWordC#_carry",
            PrimOpKind::AddWordCVal => "addWordC#_val",
            PrimOpKind::AddWordCCarry => "addWordC#_carry",
            PrimOpKind::TimesWord2Hi => "timesWord2#_hi",
            PrimOpKind::TimesWord2Lo => "timesWord2#_lo",
            PrimOpKind::QuotRemWordVal => "quotRemWord#_val",
            PrimOpKind::QuotRemWordRem => "quotRemWord#_rem",
            PrimOpKind::FfiStrlen => "ffi_strlen",
            PrimOpKind::FfiTextMeasureOff => "ffi_text_measure_off",
            PrimOpKind::FfiTextMemchr => "ffi_text_memchr",
            PrimOpKind::FfiTextReverse => "ffi_text_reverse",
            PrimOpKind::NewSmallArray => "newSmallArray#",
            PrimOpKind::ReadSmallArray => "readSmallArray#",
            PrimOpKind::WriteSmallArray => "writeSmallArray#",
            PrimOpKind::IndexSmallArray => "indexSmallArray#",
            PrimOpKind::SizeofSmallArray => "sizeofSmallArray#",
            PrimOpKind::SizeofSmallMutableArray => "getSizeofSmallMutableArray#",
            PrimOpKind::UnsafeFreezeSmallArray => "unsafeFreezeSmallArray#",
            PrimOpKind::UnsafeThawSmallArray => "unsafeThawSmallArray#",
            PrimOpKind::CopySmallArray => "copySmallArray#",
            PrimOpKind::CopySmallMutableArray => "copySmallMutableArray#",
            PrimOpKind::CloneSmallArray => "cloneSmallArray#",
            PrimOpKind::CloneSmallMutableArray => "cloneSmallMutableArray#",
            PrimOpKind::ShrinkSmallMutableArray => "shrinkSmallMutableArray#",
            PrimOpKind::NewArray => "newArray#",
            PrimOpKind::ReadArray => "readArray#",
            PrimOpKind::WriteArray => "writeArray#",
            PrimOpKind::SizeofArray => "sizeofArray#",
            PrimOpKind::SizeofMutableArray => "sizeofMutableArray#",
            PrimOpKind::UnsafeFreezeArray => "unsafeFreezeArray#",
            PrimOpKind::UnsafeThawArray => "unsafeThawArray#",
            PrimOpKind::CopyArray => "copyArray#",
            PrimOpKind::CopyMutableArray => "copyMutableArray#",
            PrimOpKind::CloneArray => "cloneArray#",
            PrimOpKind::CloneMutableArray => "cloneMutableArray#",
            PrimOpKind::PopCnt => "popCnt#",
            PrimOpKind::PopCnt8 => "popCnt8#",
            PrimOpKind::PopCnt16 => "popCnt16#",
            PrimOpKind::PopCnt32 => "popCnt32#",
            PrimOpKind::PopCnt64 => "popCnt64#",
            PrimOpKind::Ctz => "ctz#",
            PrimOpKind::Ctz8 => "ctz8#",
            PrimOpKind::Ctz16 => "ctz16#",
            PrimOpKind::Ctz32 => "ctz32#",
            PrimOpKind::Ctz64 => "ctz64#",
            PrimOpKind::CasSmallArray => "casSmallArray#",
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
