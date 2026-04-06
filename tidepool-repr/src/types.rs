//! Core type definitions for Tidepool IR identifiers and literals.

/// Tag byte stored in high bits of VarId to mark error-sentinel bindings.
pub const ERROR_SENTINEL_TAG: u8 = 0x45;

/// Variable identifier. Wraps a numeric ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VarId(pub u64);

/// Join point label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JoinId(pub u64);

/// Data constructor identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DataConId(pub u64);

/// Literal values. Matches GHC's post-O2 literal types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Literal {
    /// 64-bit signed integer.
    LitInt(i64),
    /// 64-bit unsigned integer.
    LitWord(u64),
    /// Unicode character.
    LitChar(char),
    /// UTF-8 or raw byte string.
    LitString(Vec<u8>),
    /// 32-bit floating point (stored as IEEE 754 bits).
    LitFloat(u64),
    /// 64-bit floating point (stored as IEEE 754 bits).
    LitDouble(u64),
}

macro_rules! define_primops {
    ( $( $variant:ident => $serial:literal, $display:literal; )* ) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        /// Kind of primitive operation.
        pub enum PrimOpKind {
            $( $variant, )*
        }

        impl std::fmt::Display for PrimOpKind {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                let s = match self {
                    $( PrimOpKind::$variant => $display, )*
                };
                write!(f, "{}", s)
            }
        }

        impl PrimOpKind {
            /// All variants of PrimOpKind.
            pub const ALL_VARIANTS: &'static [Self] = &[
                $( Self::$variant, )*
            ];

            /// Name used in CBOR serialization.
            pub fn serial_name(&self) -> &'static str {
                match self {
                    $( PrimOpKind::$variant => $serial, )*
                }
            }

            /// Parse from CBOR serialization name.
            pub fn from_serial_name(s: &str) -> Option<Self> {
                match s {
                    $( $serial => Some(PrimOpKind::$variant), )*
                    _ => None,
                }
            }
        }

        impl std::str::FromStr for PrimOpKind {
            type Err = String;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::from_serial_name(s).ok_or_else(|| format!("unknown primop: {}", s))
            }
        }
    };
}

define_primops! {
    IntAdd => "IntAdd", "+#";
    IntSub => "IntSub", "-#";
    IntMul => "IntMul", "*#";
    IntNegate => "IntNegate", "negateInt#";
    IntEq => "IntEq", "==#";
    IntNe => "IntNe", "/=#";
    IntLt => "IntLt", "<#";
    IntLe => "IntLe", "<=#";
    IntGt => "IntGt", ">#";
    IntGe => "IntGe", ">=#";
    WordAdd => "WordAdd", "plusWord#";
    WordSub => "WordSub", "minusWord#";
    WordMul => "WordMul", "timesWord#";
    WordEq => "WordEq", "eqWord#";
    WordNe => "WordNe", "neWord#";
    WordLt => "WordLt", "ltWord#";
    WordLe => "WordLe", "leWord#";
    WordGt => "WordGt", "gtWord#";
    WordGe => "WordGe", "geWord#";
    DoubleAdd => "DoubleAdd", "+##";
    DoubleSub => "DoubleSub", "-##";
    DoubleMul => "DoubleMul", "*##";
    DoubleDiv => "DoubleDiv", "/##";
    DoubleEq => "DoubleEq", "==##";
    DoubleNe => "DoubleNe", "/=##";
    DoubleLt => "DoubleLt", "<##";
    DoubleLe => "DoubleLe", "<=##";
    DoubleGt => "DoubleGt", ">##";
    DoubleGe => "DoubleGe", ">=##";
    CharEq => "CharEq", "eqChar#";
    CharNe => "CharNe", "neChar#";
    CharLt => "CharLt", "ltChar#";
    CharLe => "CharLe", "leChar#";
    CharGt => "CharGt", "gtChar#";
    CharGe => "CharGe", "geChar#";
    IndexArray => "IndexArray", "indexArray#";
    SeqOp => "SeqOp", "seq";
    TagToEnum => "TagToEnum", "tagToEnum#";
    DataToTag => "DataToTag", "dataToTag#";
    IntQuot => "IntQuot", "quotInt#";
    IntRem => "IntRem", "remInt#";
    DecodeDoubleMantissa => "DecodeDoubleMantissa", "decodeDouble_Int64#[mantissa]";
    DecodeDoubleExponent => "DecodeDoubleExponent", "decodeDouble_Int64#[exponent]";
    Chr => "Chr", "chr#";
    Ord => "Ord", "ord#";
    IntAnd => "IntAnd", "andI#";
    IntOr => "IntOr", "orI#";
    IntXor => "IntXor", "xorI#";
    IntNot => "IntNot", "notI#";
    IntShl => "IntShl", "uncheckedIShiftL#";
    IntShra => "IntShra", "uncheckedIShiftRA#";
    IntShrl => "IntShrl", "uncheckedIShiftRL#";
    WordQuot => "WordQuot", "quotWord#";
    WordRem => "WordRem", "remWord#";
    WordAnd => "WordAnd", "and#";
    WordOr => "WordOr", "or#";
    WordXor => "WordXor", "xor#";
    WordNot => "WordNot", "not#";
    WordShl => "WordShl", "uncheckedShiftL#";
    WordShrl => "WordShrl", "uncheckedShiftRL#";
    Int2Word => "Int2Word", "int2Word#";
    Word2Int => "Word2Int", "word2Int#";
    Narrow8Int => "Narrow8Int", "narrow8Int#";
    Narrow16Int => "Narrow16Int", "narrow16Int#";
    Narrow32Int => "Narrow32Int", "narrow32Int#";
    Narrow8Word => "Narrow8Word", "narrow8Word#";
    Narrow16Word => "Narrow16Word", "narrow16Word#";
    Narrow32Word => "Narrow32Word", "narrow32Word#";
    FloatAdd => "FloatAdd", "plusFloat#";
    FloatSub => "FloatSub", "minusFloat#";
    FloatMul => "FloatMul", "timesFloat#";
    FloatDiv => "FloatDiv", "divideFloat#";
    FloatNegate => "FloatNegate", "negateFloat#";
    FloatEq => "FloatEq", "eqFloat#";
    FloatNe => "FloatNe", "neFloat#";
    FloatLt => "FloatLt", "ltFloat#";
    FloatLe => "FloatLe", "leFloat#";
    FloatGt => "FloatGt", "gtFloat#";
    FloatGe => "FloatGe", "geFloat#";
    DoubleNegate => "DoubleNegate", "negateDouble#";
    DoubleFabs => "DoubleFabs", "fabsDouble#";
    DoubleSqrt => "DoubleSqrt", "sqrtDouble#";
    DoubleExp => "DoubleExp", "expDouble#";
    DoubleExpM1 => "DoubleExpM1", "expm1Double#";
    DoubleLog => "DoubleLog", "logDouble#";
    DoubleLog1P => "DoubleLog1P", "log1pDouble#";
    DoubleSin => "DoubleSin", "sinDouble#";
    DoubleCos => "DoubleCos", "cosDouble#";
    DoubleTan => "DoubleTan", "tanDouble#";
    DoubleAsin => "DoubleAsin", "asinDouble#";
    DoubleAcos => "DoubleAcos", "acosDouble#";
    DoubleAtan => "DoubleAtan", "atanDouble#";
    DoubleSinh => "DoubleSinh", "sinhDouble#";
    DoubleCosh => "DoubleCosh", "coshDouble#";
    DoubleTanh => "DoubleTanh", "tanhDouble#";
    DoubleAsinh => "DoubleAsinh", "asinhDouble#";
    DoubleAcosh => "DoubleAcosh", "acoshDouble#";
    DoubleAtanh => "DoubleAtanh", "atanhDouble#";
    DoublePower => "DoublePower", "**##";
    Int2Double => "Int2Double", "int2Double#";
    Double2Int => "Double2Int", "double2Int#";
    Int2Float => "Int2Float", "int2Float#";
    Float2Int => "Float2Int", "float2Int#";
    Double2Float => "Double2Float", "double2Float#";
    Float2Double => "Float2Double", "float2Double#";
    ReallyUnsafePtrEquality => "ReallyUnsafePtrEquality", "reallyUnsafePtrEquality#";
    IndexCharOffAddr => "IndexCharOffAddr", "indexCharOffAddr#";
    PlusAddr => "PlusAddr", "plusAddr#";
    Raise => "Raise", "raise#";
    NewByteArray => "NewByteArray", "newByteArray#";
    ReadWord8Array => "ReadWord8Array", "readWord8Array#";
    WriteWord8Array => "WriteWord8Array", "writeWord8Array#";
    SizeofMutableByteArray => "SizeofMutableByteArray", "sizeofMutableByteArray#";
    UnsafeFreezeByteArray => "UnsafeFreezeByteArray", "unsafeFreezeByteArray#";
    CopyByteArray => "CopyByteArray", "copyByteArray#";
    CopyMutableByteArray => "CopyMutableByteArray", "copyMutableByteArray#";
    CopyAddrToByteArray => "CopyAddrToByteArray", "copyAddrToByteArray#";
    ShrinkMutableByteArray => "ShrinkMutableByteArray", "shrinkMutableByteArray#";
    ResizeMutableByteArray => "ResizeMutableByteArray", "resizeMutableByteArray#";
    Clz8 => "Clz8", "clz8#";
    IntToInt64 => "IntToInt64", "intToInt64#";
    Int64ToWord64 => "Int64ToWord64", "int64ToWord64#";
    TimesInt2Hi => "TimesInt2Hi", "timesInt2Hi";
    TimesInt2Lo => "TimesInt2Lo", "timesInt2Lo";
    TimesInt2Overflow => "TimesInt2Overflow", "timesInt2Overflow";
    IndexWord8Array => "IndexWord8Array", "indexWord8Array#";
    IndexWord8OffAddr => "IndexWord8OffAddr", "indexWord8OffAddr#";
    CompareByteArrays => "CompareByteArrays", "compareByteArrays#";
    WordToWord8 => "WordToWord8", "wordToWord8#";
    Word64And => "Word64And", "andWord64#";
    Int64ToInt => "Int64ToInt", "int64ToInt#";
    Word64ToInt64 => "Word64ToInt64", "word64ToInt64#";
    Word8ToWord => "Word8ToWord", "word8ToWord#";
    Word8Lt => "Word8Lt", "ltWord8#";
    Int64Ge => "Int64Ge", "geInt64#";
    Int64Negate => "Int64Negate", "negateInt64#";
    Int64Shra => "Int64Shra", "uncheckedIShiftRA64#";
    Word64Shl => "Word64Shl", "uncheckedShiftL64#";
    Word8Ge => "Word8Ge", "geWord8#";
    Word8Sub => "Word8Sub", "subWord8#";
    SizeofByteArray => "SizeofByteArray", "sizeofByteArray#";
    IndexWordArray => "IndexWordArray", "indexWordArray#";
    Int64Add => "Int64Add", "plusInt64#";
    Int64Gt => "Int64Gt", "gtInt64#";
    Int64Mul => "Int64Mul", "timesInt64#";
    Int64Lt => "Int64Lt", "ltInt64#";
    Int64Le => "Int64Le", "leInt64#";
    Int64Sub => "Int64Sub", "subInt64#";
    Int64Shl => "Int64Shl", "uncheckedIShiftL64#";
    WriteWordArray => "WriteWordArray", "writeWordArray#";
    ReadWordArray => "ReadWordArray", "readWordArray#";
    SetByteArray => "SetByteArray", "setByteArray#";
    Word64Or => "Word64Or", "or64#";
    Word8Add => "Word8Add", "plusWord8#";
    Word8Le => "Word8Le", "leWord8#";
    AddIntCVal => "AddIntCVal", "addIntC#_val";
    AddIntCCarry => "AddIntCCarry", "addIntC#_overflow";
    SubWordCVal => "SubWordCVal", "subWordC#_val";
    SubWordCCarry => "SubWordCCarry", "subWordC#_carry";
    AddWordCVal => "AddWordCVal", "addWordC#_val";
    AddWordCCarry => "AddWordCCarry", "addWordC#_carry";
    TimesWord2Hi => "TimesWord2Hi", "timesWord2#_hi";
    TimesWord2Lo => "TimesWord2Lo", "timesWord2#_lo";
    QuotRemWordVal => "QuotRemWordVal", "quotRemWord#_val";
    QuotRemWordRem => "QuotRemWordRem", "quotRemWord#_rem";
    FfiStrlen => "FfiStrlen", "ffi_strlen";
    FfiTextMeasureOff => "FfiTextMeasureOff", "ffi_text_measure_off";
    FfiTextMemchr => "FfiTextMemchr", "ffi_text_memchr";
    FfiTextReverse => "FfiTextReverse", "ffi_text_reverse";
    NewSmallArray => "NewSmallArray", "newSmallArray#";
    ReadSmallArray => "ReadSmallArray", "readSmallArray#";
    WriteSmallArray => "WriteSmallArray", "writeSmallArray#";
    IndexSmallArray => "IndexSmallArray", "indexSmallArray#";
    SizeofSmallArray => "SizeofSmallArray", "sizeofSmallArray#";
    SizeofSmallMutableArray => "SizeofSmallMutableArray", "getSizeofSmallMutableArray#";
    UnsafeFreezeSmallArray => "UnsafeFreezeSmallArray", "unsafeFreezeSmallArray#";
    UnsafeThawSmallArray => "UnsafeThawSmallArray", "unsafeThawSmallArray#";
    CopySmallArray => "CopySmallArray", "copySmallArray#";
    CopySmallMutableArray => "CopySmallMutableArray", "copySmallMutableArray#";
    CloneSmallArray => "CloneSmallArray", "cloneSmallArray#";
    CloneSmallMutableArray => "CloneSmallMutableArray", "cloneSmallMutableArray#";
    ShrinkSmallMutableArray => "ShrinkSmallMutableArray", "shrinkSmallMutableArray#";
    NewArray => "NewArray", "newArray#";
    ReadArray => "ReadArray", "readArray#";
    WriteArray => "WriteArray", "writeArray#";
    SizeofArray => "SizeofArray", "sizeofArray#";
    SizeofMutableArray => "SizeofMutableArray", "sizeofMutableArray#";
    UnsafeFreezeArray => "UnsafeFreezeArray", "unsafeFreezeArray#";
    UnsafeThawArray => "UnsafeThawArray", "unsafeThawArray#";
    CopyArray => "CopyArray", "copyArray#";
    CopyMutableArray => "CopyMutableArray", "copyMutableArray#";
    CloneArray => "CloneArray", "cloneArray#";
    CloneMutableArray => "CloneMutableArray", "cloneMutableArray#";
    PopCnt => "PopCnt", "popCnt#";
    PopCnt8 => "PopCnt8", "popCnt8#";
    PopCnt16 => "PopCnt16", "popCnt16#";
    PopCnt32 => "PopCnt32", "popCnt32#";
    PopCnt64 => "PopCnt64", "popCnt64#";
    Ctz => "Ctz", "ctz#";
    Ctz8 => "Ctz8", "ctz8#";
    Ctz16 => "Ctz16", "ctz16#";
    Ctz32 => "Ctz32", "ctz32#";
    Ctz64 => "Ctz64", "ctz64#";
    CasSmallArray => "CasSmallArray", "casSmallArray#";
    ShowDoubleAddr => "ShowDoubleAddr", "showDoubleAddr";
}

/// Case alternative constructor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AltCon {
    /// A data constructor pattern.
    DataAlt(DataConId),
    /// A literal pattern.
    LitAlt(Literal),
    /// The default case (_).
    Default,
}

/// A case alternative: constructor pattern + bound variables + body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alt<A> {
    /// The pattern constructor.
    pub con: AltCon,
    /// Variables bound by this pattern.
    pub binders: Vec<VarId>,
    /// The body of the alternative.
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
    fn test_primop_serial_invariant() {
        for op in PrimOpKind::ALL_VARIANTS {
            let name = op.serial_name();
            let recovered = PrimOpKind::from_serial_name(name);
            assert_eq!(
                recovered,
                Some(*op),
                "PrimOpKind variant {:?} failed round-trip through serial name '{}'",
                op,
                name
            );

            // Test FromStr
            let from_str: PrimOpKind = name.parse().unwrap();
            assert_eq!(from_str, *op);
        }
    }

    #[test]
    fn test_primop_from_str_error() {
        let res: Result<PrimOpKind, _> = "NoSuchOp".parse();
        assert!(res.is_err());
        assert_eq!(res.unwrap_err(), "unknown primop: NoSuchOp");
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
