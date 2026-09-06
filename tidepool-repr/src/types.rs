//! Core type definitions for Tidepool IR identifiers and literals.

/// Tag byte stored in high bits of VarId to mark error-sentinel bindings.
pub const ERROR_SENTINEL_TAG: u8 = 0x45;

/// High-byte tag marking an external (Option-C session/library) binder id.
/// A real external under Option C: `stableVarId = 0xFE<<56 | fingerprint`.
pub const EXTERNAL_TAG: u8 = 0xFE;

/// Variable identifier. Wraps a numeric ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VarId(pub u64);

/// The decoded payload of a `0x45` error-sentinel [`VarId`].
///
/// Layout (`Translate.errorSentinelVar`): `0x45 << 56 | slot << 8 | kind`.
/// The kind stays in the LOW byte, so sentinels that carry no slot are
/// byte-identical to the pre-slot encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SentinelPayload {
    /// Which sentinel this is: 0 = div-by-zero, 1 = overflow, 2 = error,
    /// 3 = undefined, 4 = type metadata / unresolved-external poison.
    pub kind: u8,
    /// Per-module identity slot of the symbol this sentinel REPLACED, or `0`
    /// when the sentinel records no identity (every kind but the
    /// unresolved-external poison, plus payloads from pre-2.1 extractors).
    /// Resolved to a qualified name through `meta.cbor`'s `poisoned` table.
    pub slot: u64,
}

/// Decoded high-byte tag of a [`VarId`]. Replaces bare byte
/// comparisons (`v >> 56 == 0x..`) at resolution sites with an exhaustive match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarKind {
    /// `0xFE` — a real external (library unfolding OR a session binder).
    External,
    /// `0x45` — an error/undefined/type-metadata sentinel (`Translate.hs`).
    ErrorSentinel,
    /// Any other high byte — an ordinary local binder.
    Local,
}

impl VarId {
    /// The high byte of the id (`self.0 >> 56`).
    #[must_use]
    pub fn tag(self) -> u8 {
        (self.0 >> 56) as u8
    }

    /// Decode the high-byte tag into a [`VarKind`].
    #[must_use]
    pub fn kind(self) -> VarKind {
        match self.tag() {
            EXTERNAL_TAG => VarKind::External,
            ERROR_SENTINEL_TAG => VarKind::ErrorSentinel,
            _ => VarKind::Local,
        }
    }

    /// Decode an error sentinel's kind byte and identity slot; `None` for any
    /// id that isn't `0x45`-tagged. The ONE place the sentinel bit layout is
    /// decoded — every consumer (the eval oracle, the JIT's poison emission)
    /// goes through here rather than open-coding the shifts.
    #[must_use]
    pub fn sentinel(self) -> Option<SentinelPayload> {
        if self.tag() != ERROR_SENTINEL_TAG {
            return None;
        }
        Some(SentinelPayload {
            kind: (self.0 & 0xFF) as u8,
            slot: (self.0 >> 8) & 0xFFFF_FFFF_FFFF,
        })
    }
}

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
    /// Raw `ByteArray#` literal (e.g. a `BigNat#` payload): the bytes ARE the
    /// array contents. Distinct from `LitString` so it lowers with the
    /// ByteArray# layout (`sizeofByteArray#` reads the length prefix; no
    /// unpackCString# `+8` adjustment) instead of the string layout.
    LitByteArray(Vec<u8>),
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
    DecodeFloatMantissa => "DecodeFloatMantissa", "decodeFloat_Int#[mantissa]";
    DecodeFloatExponent => "DecodeFloatExponent", "decodeFloat_Int#[exponent]";
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
    // Float unary math with direct hardware opcodes (parallel to DoubleSqrt /
    // DoubleFabs). The transcendental Float ops (expFloat#, sinFloat#, …) have no
    // hardware opcode and are desugared in Translate.hs to the Double libm path
    // (float2Double# → *Double# → double2Float#), so they need no variant here.
    FloatSqrt => "FloatSqrt", "sqrtFloat#";
    FloatFabs => "FloatFabs", "fabsFloat#";
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
    Word2Double => "Word2Double", "word2Double#";
    Double2Int => "Double2Int", "double2Int#";
    Int2Float => "Int2Float", "int2Float#";
    Float2Int => "Float2Int", "float2Int#";
    Double2Float => "Double2Float", "double2Float#";
    Float2Double => "Float2Double", "float2Double#";
    ReallyUnsafePtrEquality => "ReallyUnsafePtrEquality", "reallyUnsafePtrEquality#";
    IndexCharOffAddr => "IndexCharOffAddr", "indexCharOffAddr#";
    PlusAddr => "PlusAddr", "plusAddr#";
    Raise => "Raise", "raise#";
    // Arithmetic-exception primops (GHC inserts these in ghc-bignum's underflow /
    // divide-by-zero / overflow check branches). Bottoming; lowered to a runtime error.
    RaiseUnderflow => "RaiseUnderflow", "raiseUnderflow#";
    RaiseOverflow => "RaiseOverflow", "raiseOverflow#";
    RaiseDivZero => "RaiseDivZero", "raiseDivZero#";
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
    Clz => "Clz", "clz#";
    IntToInt64 => "IntToInt64", "intToInt64#";
    Int64ToWord64 => "Int64ToWord64", "int64ToWord64#";
    TimesInt2Hi => "TimesInt2Hi", "timesInt2Hi";
    TimesInt2Lo => "TimesInt2Lo", "timesInt2Lo";
    TimesInt2Overflow => "TimesInt2Overflow", "timesInt2Overflow";
    IndexWord8Array => "IndexWord8Array", "indexWord8Array#";
    IndexWord8OffAddr => "IndexWord8OffAddr", "indexWord8OffAddr#";
    WriteWord8OffAddr => "WriteWord8OffAddr", "writeWord8OffAddr#";
    ByteArrayContents => "ByteArrayContents", "byteArrayContents#";
    CompareByteArrays => "CompareByteArrays", "compareByteArrays#";
    WordToWord8 => "WordToWord8", "wordToWord8#";
    Word64And => "Word64And", "andWord64#";
    Int64ToInt => "Int64ToInt", "int64ToInt#";
    Word64ToInt64 => "Word64ToInt64", "word64ToInt64#";
    Word64ToWord => "Word64ToWord", "word64ToWord#";
    WordToWord64 => "WordToWord64", "wordToWord64#";
    Word8ToWord => "Word8ToWord", "word8ToWord#";
    Word8Lt => "Word8Lt", "ltWord8#";
    Int64Ge => "Int64Ge", "geInt64#";
    Int64Negate => "Int64Negate", "negateInt64#";
    Int64Shra => "Int64Shra", "uncheckedIShiftRA64#";
    Word64Shl => "Word64Shl", "uncheckedShiftL64#";
    Word64Shrl => "Word64Shrl", "uncheckedShiftRL64#";
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
    Word64Eq => "Word64Eq", "eqWord64#";
    Word64Ne => "Word64Ne", "neWord64#";
    Word64Lt => "Word64Lt", "ltWord64#";
    Word64Le => "Word64Le", "leWord64#";
    Word64Gt => "Word64Gt", "gtWord64#";
    Word64Ge => "Word64Ge", "geWord64#";
    Word64Add => "Word64Add", "plusWord64#";
    Word64Sub => "Word64Sub", "subWord64#";
    Word64Mul => "Word64Mul", "timesWord64#";
    Word64Quot => "Word64Quot", "quotWord64#";
    Word64Rem => "Word64Rem", "remWord64#";
    Word64Xor => "Word64Xor", "xor64#";
    Word64Not => "Word64Not", "not64#";
    Int64Quot => "Int64Quot", "quotInt64#";
    Int64Rem => "Int64Rem", "remInt64#";
    Int64Shrl => "Int64Shrl", "uncheckedIShiftRL64#";
    Int64Eq => "Int64Eq", "eqInt64#";
    Int64Ne => "Int64Ne", "neInt64#";
    Word8Add => "Word8Add", "plusWord8#";
    Word8Le => "Word8Le", "leWord8#";
    AddIntCVal => "AddIntCVal", "addIntC#_val";
    AddIntCCarry => "AddIntCCarry", "addIntC#_overflow";
    SubWordCVal => "SubWordCVal", "subWordC#_val";
    SubWordCCarry => "SubWordCCarry", "subWordC#_carry";
    SubIntCVal => "SubIntCVal", "subIntC#_val";
    SubIntCCarry => "SubIntCCarry", "subIntC#_overflow";
    AddWordCVal => "AddWordCVal", "addWordC#_val";
    AddWordCCarry => "AddWordCCarry", "addWordC#_carry";
    TimesWord2Hi => "TimesWord2Hi", "timesWord2#_hi";
    TimesWord2Lo => "TimesWord2Lo", "timesWord2#_lo";
    // plusWord2# :: Word# -> Word# -> (# high, low #) — the native ghc-bignum
    // backend's add-with-carry. quotRemWord2# :: (high, low, divisor) -> (# q, r #)
    // — its 128/64 division primitive (the core of multi-precision division).
    WordAdd2Hi => "WordAdd2Hi", "plusWord2#_hi";
    WordAdd2Lo => "WordAdd2Lo", "plusWord2#_lo";
    WordQuotRem2Quot => "WordQuotRem2Quot", "quotRemWord2#_quot";
    WordQuotRem2Rem => "WordQuotRem2Rem", "quotRemWord2#_rem";
    QuotRemWordVal => "QuotRemWordVal", "quotRemWord#_val";
    QuotRemWordRem => "QuotRemWordRem", "quotRemWord#_rem";
    FfiStrlen => "FfiStrlen", "ffi_strlen";
    FfiRintDouble => "FfiRintDouble", "ffi_rint_double";
    // GHC classification FFI: one matching Float#/Double# argument, Int# 0 or 1.
    FfiIsFloatNaN => "FfiIsFloatNaN", "ffi_is_float_nan";
    FfiIsFloatInfinite => "FfiIsFloatInfinite", "ffi_is_float_infinite";
    FfiIsFloatNegativeZero => "FfiIsFloatNegativeZero", "ffi_is_float_negative_zero";
    FfiIsDoubleNaN => "FfiIsDoubleNaN", "ffi_is_double_nan";
    FfiIsDoubleInfinite => "FfiIsDoubleInfinite", "ffi_is_double_infinite";
    FfiIsDoubleNegativeZero => "FfiIsDoubleNegativeZero", "ffi_is_double_negative_zero";
    FfiTextMeasureOff => "FfiTextMeasureOff", "ffi_text_measure_off";
    FfiTextMemchr => "FfiTextMemchr", "ffi_text_memchr";
    FfiTextReverse => "FfiTextReverse", "ffi_text_reverse";
    // __int_encodeDouble(mantissa, exp) -> Double# (ldexp); the final Integer->Double step.
    // The only surviving ghc-bignum FFI: under the native backend, all other
    // Integer/Natural arithmetic is pure Core over Word#/ByteArray# primops.
    FfiIntEncodeDouble => "FfiIntEncodeDouble", "int_encode_double";
    FfiWordEncodeDouble => "FfiWordEncodeDouble", "word_encode_double";
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
    // Render a Double directly to managed `Text`.
    RenderDoubleText => "RenderDoubleText", "renderDoubleText";
    // Precedence-aware managed-`Text` rendering.
    RenderDoublePrecText => "RenderDoublePrecText", "renderDoublePrecText";
    // Sized Word8/Int8/Word32 (on 64-bit these are masked Int#/Word#).
    Word8Gt => "Word8Gt", "gtWord8#";
    Word8Quot => "Word8Quot", "quotWord8#";
    Word8Rem => "Word8Rem", "remWord8#";
    Word8Mul => "Word8Mul", "timesWord8#";
    Int8ToInt => "Int8ToInt", "int8ToInt#";
    Int8ToWord8 => "Int8ToWord8", "int8ToWord8#";
    Word8ToInt8 => "Word8ToInt8", "word8ToInt8#";
    Int8Negate => "Int8Negate", "negateInt8#";
    Int32ToInt => "Int32ToInt", "int32ToInt#";
    Word32ToWord => "Word32ToWord", "word32ToWord#";
    WordToWord32 => "WordToWord32", "wordToWord32#";
    Word32Gt => "Word32Gt", "gtWord32#";
    Word32Le => "Word32Le", "leWord32#";
    Word32Lt => "Word32Lt", "ltWord32#";
    Word32Add => "Word32Add", "plusWord32#";
    Word32Sub => "Word32Sub", "subWord32#";
    // Addr#
    EqAddr => "EqAddr", "eqAddr#";
    MinusAddr => "MinusAddr", "minusAddr#";
    IndexAddrArray => "IndexAddrArray", "indexAddrArray#";
    IndexAddrOffAddr => "IndexAddrOffAddr", "indexAddrOffAddr#";
    IndexInt8OffAddr => "IndexInt8OffAddr", "indexInt8OffAddr#";
    IndexWord32OffAddr => "IndexWord32OffAddr", "indexWord32OffAddr#";
    IndexWideCharOffAddr => "IndexWideCharOffAddr", "indexWideCharOffAddr#";
    WriteWideCharOffAddr => "WriteWideCharOffAddr", "writeWideCharOffAddr#";
    // Pure JSON decode: `eitherDecodeValue :: Text -> Either Text Value`. Dispatches to Rust
    // serde_json and builds the vendored aeson `Value` ADT on the heap (Rust
    // side in `tidepool-eval::json` / the `runtime_json_decode` JIT host fn).
    // Not a real GHC primop; surfaced via Translate.hs binding interception.
    JsonDecode => "JsonDecode", "jsonDecode#";
    // Pure ISO-8601/RFC-3339 parse: `parseISO8601 :: Text -> Either Text UTCTime`.
    // Dispatches to Rust `chrono` and builds the `Either Text UTCTime` ADT
    // (`tidepool-eval::json::parse_iso8601_str` / the `runtime_parse_iso8601` JIT
    // host fn). Not a real GHC primop; surfaced via Translate.hs interception.
    ParseISO8601 => "ParseISO8601", "parseISO8601#";
}

/// Evaluation required for a primitive argument before executing the operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimArgDemand {
    /// Evaluate to WHNF; numeric/address operations subsequently validate shape.
    Strict,
    /// Pass a heap value without entering it (for lifted array elements).
    Lazy,
}

impl PrimOpKind {
    /// Argument positions are zero-based in the lowered IR, after State# erasure.
    /// Demand belongs to the operation, not to an emitter's traversal strategy.
    pub fn argument_demand(self, position: usize) -> PrimArgDemand {
        match (self, position) {
            (Self::NewArray | Self::NewSmallArray, 1)
            | (Self::WriteArray | Self::WriteSmallArray, 2)
            | (Self::CasSmallArray, 2 | 3) => PrimArgDemand::Lazy,
            _ => PrimArgDemand::Strict,
        }
    }
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
            Literal::LitByteArray(bs) => write!(f, "<bytearray len={}>", bs.len()),
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
#[allow(clippy::approx_constant)] // tests use 3.14 as a round-trip float literal
mod tests {
    use super::*;
    use crate::builder::TreeBuilder;
    use crate::frame::CoreFrame;

    #[test]
    fn test_var_id_display() {
        assert_eq!(VarId(42).to_string(), "v_42");
    }

    /// The sentinel layout (`0x45<<56 | slot<<8 | kind`): the kind stays in the
    /// LOW byte, so a slotless sentinel decodes exactly as the pre-slot
    /// encoding did, and a slot-carrying poison decodes both halves.
    #[test]
    fn sentinel_decodes_kind_and_slot() {
        for kind in 0u8..=4 {
            let slotless = VarId(0x4500_0000_0000_0000 | u64::from(kind));
            let p = slotless.sentinel().expect("0x45-tagged");
            assert_eq!((p.kind, p.slot), (kind, 0));
        }
        let poisoned = VarId(0x4500_0000_0000_0000 | (7u64 << 8) | 4);
        let p = poisoned.sentinel().expect("0x45-tagged");
        assert_eq!((p.kind, p.slot), (4, 7));
        // The maximum representable slot uses all 48 middle bits.
        let wide = VarId(0x4500_0000_0000_0000 | (0xFFFF_FFFF_FFFF << 8) | 4);
        assert_eq!(wide.sentinel().map(|p| p.slot), Some(0xFFFF_FFFF_FFFF));
        // Non-sentinel tags decode to None, not to a bogus kind/slot.
        assert!(VarId(0xFE00_0000_0000_0004).sentinel().is_none());
        assert!(VarId(42).sentinel().is_none());
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
