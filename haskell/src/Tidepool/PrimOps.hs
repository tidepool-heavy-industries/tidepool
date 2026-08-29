module Tidepool.PrimOps
  ( floatMathToDouble
  , mapPrimOp
  , primOpArity
  , splitMultiReturnPrimOp
  , splitTripleReturnPrimOp
  , splitUnaryMultiReturnPrimOp
  , splitWord2DivPrimOp
  ) where

import Data.Text (Text)
import qualified Data.Text as T
import GHC.Builtin.PrimOps
import GHC.Utils.Outputable (showPprUnsafe)

mapPrimOp :: PrimOp -> Text
mapPrimOp = \case
  IntAddOp    -> "IntAdd"
  IntSubOp    -> "IntSub"
  IntMulOp    -> "IntMul"
  IntNegOp    -> "IntNegate"
  IntEqOp     -> "IntEq"
  IntNeOp     -> "IntNe"
  IntLtOp     -> "IntLt"
  IntLeOp     -> "IntLe"
  IntGtOp     -> "IntGt"
  IntGeOp     -> "IntGe"
  WordAddOp   -> "WordAdd"
  WordSubOp   -> "WordSub"
  WordMulOp   -> "WordMul"
  WordEqOp    -> "WordEq"
  WordNeOp    -> "WordNe"
  WordLtOp    -> "WordLt"
  WordLeOp    -> "WordLe"
  WordGtOp    -> "WordGt"
  WordGeOp    -> "WordGe"
  DoubleAddOp -> "DoubleAdd"
  DoubleSubOp -> "DoubleSub"
  DoubleMulOp -> "DoubleMul"
  DoubleDivOp -> "DoubleDiv"
  DoubleEqOp  -> "DoubleEq"
  DoubleNeOp  -> "DoubleNe"
  DoubleLtOp  -> "DoubleLt"
  DoubleLeOp  -> "DoubleLe"
  DoubleGtOp  -> "DoubleGt"
  DoubleGeOp  -> "DoubleGe"
  CharEqOp    -> "CharEq"
  CharNeOp    -> "CharNe"
  CharLtOp    -> "CharLt"
  CharLeOp    -> "CharLe"
  CharGtOp    -> "CharGt"
  CharGeOp    -> "CharGe"
  IndexArrayOp -> "IndexArray"
  TagToEnumOp -> "TagToEnum"
  -- No DataToTagSmallOp/DataToTagLargeOp arm on purpose: `translateHead`
  -- desugars both into a `case` over the type's constructors, because the
  -- primop's answer is the constructor's index within its own data type and
  -- the backends' `DataToTag` yields the runtime constructor tag instead.
  -- Falling through to the generic `Unsupported primop` error is the point —
  -- an occurrence that arm cannot reach must be loud, not silently garbage.
  IntQuotOp -> "IntQuot"
  IntRemOp  -> "IntRem"
  ChrOp     -> "Chr"
  OrdOp     -> "Ord"
  -- Int bitwise
  IntAndOp  -> "IntAnd"
  IntOrOp   -> "IntOr"
  IntXorOp  -> "IntXor"
  IntNotOp  -> "IntNot"
  IntSllOp  -> "IntShl"
  IntSraOp  -> "IntShra"
  IntSrlOp  -> "IntShrl"
  -- Word arithmetic + bitwise
  WordQuotOp -> "WordQuot"
  WordRemOp  -> "WordRem"
  WordAndOp  -> "WordAnd"
  WordOrOp   -> "WordOr"
  WordXorOp  -> "WordXor"
  WordNotOp  -> "WordNot"
  WordSllOp  -> "WordShl"
  WordSrlOp  -> "WordShrl"
  -- Int↔Word conversions
  IntToWordOp -> "Int2Word"
  WordToIntOp -> "Word2Int"
  -- Narrowing
  Narrow8IntOp   -> "Narrow8Int"
  Narrow16IntOp  -> "Narrow16Int"
  Narrow32IntOp  -> "Narrow32Int"
  Narrow8WordOp  -> "Narrow8Word"
  Narrow16WordOp -> "Narrow16Word"
  Narrow32WordOp -> "Narrow32Word"
  -- Float arithmetic + comparison
  FloatAddOp    -> "FloatAdd"
  FloatSubOp    -> "FloatSub"
  FloatMulOp    -> "FloatMul"
  FloatDivOp    -> "FloatDiv"
  FloatNegOp    -> "FloatNegate"
  FloatEqOp     -> "FloatEq"
  FloatNeOp     -> "FloatNe"
  FloatLtOp     -> "FloatLt"
  FloatLeOp     -> "FloatLe"
  FloatGtOp     -> "FloatGt"
  FloatGeOp     -> "FloatGe"
  -- Float unary math with native hardware opcodes (sqrt/fabs). The Float
  -- transcendentals (expFloat#, sinFloat#, …) have no hardware opcode and are
  -- desugared to the Double libm path in `desugarFloatMath` before reaching here.
  FloatSqrtOp   -> "FloatSqrt"
  FloatFabsOp   -> "FloatFabs"
  -- Double extras
  DoubleNegOp   -> "DoubleNegate"
  DoubleFabsOp  -> "DoubleFabs"
  -- Double math (Floating class)
  DoubleSqrtOp  -> "DoubleSqrt"
  DoubleExpOp   -> "DoubleExp"
  DoubleExpM1Op -> "DoubleExpM1"
  DoubleLogOp   -> "DoubleLog"
  DoubleLog1POp -> "DoubleLog1P"
  DoubleSinOp   -> "DoubleSin"
  DoubleCosOp   -> "DoubleCos"
  DoubleTanOp   -> "DoubleTan"
  DoubleAsinOp  -> "DoubleAsin"
  DoubleAcosOp  -> "DoubleAcos"
  DoubleAtanOp  -> "DoubleAtan"
  DoubleSinhOp  -> "DoubleSinh"
  DoubleCoshOp  -> "DoubleCosh"
  DoubleTanhOp  -> "DoubleTanh"
  DoubleAsinhOp -> "DoubleAsinh"
  DoubleAcoshOp -> "DoubleAcosh"
  DoubleAtanhOp -> "DoubleAtanh"
  DoublePowerOp -> "DoublePower"
  -- Type conversions
  IntToDoubleOp   -> "Int2Double"
  WordToDoubleOp  -> "Word2Double"
  DoubleToIntOp   -> "Double2Int"
  IntToFloatOp    -> "Int2Float"
  FloatToIntOp    -> "Float2Int"
  DoubleToFloatOp -> "Double2Float"
  FloatToDoubleOp -> "Float2Double"
  -- Pointer equality (polyfill: always 0# = not equal)
  ReallyUnsafePtrEqualityOp -> "ReallyUnsafePtrEquality"
  -- Addr#
  IndexOffAddrOp_Char -> "IndexCharOffAddr"
  AddrAddOp           -> "PlusAddr"
  AddrEqOp            -> "EqAddr"
  AddrSubOp           -> "MinusAddr"
  IndexByteArrayOp_Addr -> "IndexAddrArray"
  -- Off-addr reads (the stateful `read*OffAddr#` variants share the serial name
  -- with their pure `index*OffAddr#` siblings — the trailing State# arg is erased
  -- and the (# State#, result #) tuple collapses to just the result at emit).
  IndexOffAddrOp_Addr     -> "IndexAddrOffAddr"
  ReadOffAddrOp_Addr      -> "IndexAddrOffAddr"
  IndexOffAddrOp_Int8     -> "IndexInt8OffAddr"
  ReadOffAddrOp_Int8      -> "IndexInt8OffAddr"
  IndexOffAddrOp_Word32   -> "IndexWord32OffAddr"
  ReadOffAddrOp_Word32    -> "IndexWord32OffAddr"
  ReadOffAddrOp_Word8     -> "IndexWord8OffAddr"
  IndexOffAddrOp_WideChar -> "IndexWideCharOffAddr"
  ReadOffAddrOp_WideChar  -> "IndexWideCharOffAddr"
  WriteOffAddrOp_WideChar -> "WriteWideCharOffAddr"
  -- ByteArray#
  NewByteArrayOp_Char         -> "NewByteArray"
  -- Pinned alloc is identical to a normal ByteArray# for us; contents# yields the
  -- payload Addr#. These ride the dead Integer->Addr# serialization closure.
  NewPinnedByteArrayOp_Char        -> "NewByteArray"
  ByteArrayContents_Char           -> "ByteArrayContents"
  MutableByteArrayContents_Char    -> "ByteArrayContents"
  SizeofByteArrayOp           -> "SizeofByteArray"
  SizeofMutableByteArrayOp    -> "SizeofByteArray"
  UnsafeFreezeByteArrayOp     -> "UnsafeFreezeByteArray"
  CopyAddrToByteArrayOp       -> "CopyAddrToByteArray"
  ReadByteArrayOp_Word8       -> "ReadWord8Array"
  WriteByteArrayOp_Word8      -> "WriteWord8Array"
  IndexByteArrayOp_Word       -> "IndexWordArray"
  WriteByteArrayOp_Word       -> "WriteWordArray"
  ReadByteArrayOp_Word        -> "ReadWordArray"
  SetByteArrayOp              -> "SetByteArray"
  ShrinkMutableByteArrayOp_Char -> "ShrinkMutableByteArray"
  IndexByteArrayOp_Word8      -> "IndexWord8Array"
  IndexOffAddrOp_Word8        -> "IndexWord8OffAddr"
  WriteOffAddrOp_Word8        -> "WriteWord8OffAddr"
  CopyByteArrayOp             -> "CopyByteArray"
  CopyMutableByteArrayOp      -> "CopyMutableByteArray"
  CompareByteArraysOp         -> "CompareByteArrays"
  GetSizeofMutableByteArrayOp -> "SizeofByteArray"
  ResizeMutableByteArrayOp_Char -> "ResizeMutableByteArray"
  -- Word8
  Word8ToWordOp               -> "Word8ToWord"
  WordToWord8Op               -> "WordToWord8"
  Word8AddOp                  -> "Word8Add"
  Word8SubOp                  -> "Word8Sub"
  Word8LtOp                   -> "Word8Lt"
  Word8LeOp                   -> "Word8Le"
  Word8GeOp                   -> "Word8Ge"
  Word8GtOp                   -> "Word8Gt"
  Word8QuotOp                 -> "Word8Quot"
  Word8RemOp                  -> "Word8Rem"
  Word8MulOp                  -> "Word8Mul"
  -- Int8
  Int8ToIntOp                 -> "Int8ToInt"
  Int8ToWord8Op               -> "Int8ToWord8"
  Word8ToInt8Op               -> "Word8ToInt8"
  Int8NegOp                   -> "Int8Negate"
  -- Word32 / Int32
  Int32ToIntOp                -> "Int32ToInt"
  Word32ToWordOp              -> "Word32ToWord"
  WordToWord32Op              -> "WordToWord32"
  Word32GtOp                  -> "Word32Gt"
  Word32LeOp                  -> "Word32Le"
  Word32LtOp                  -> "Word32Lt"
  Word32AddOp                 -> "Word32Add"
  Word32SubOp                 -> "Word32Sub"
  -- Int64
  Int64AddOp                  -> "Int64Add"
  Int64SubOp                  -> "Int64Sub"
  Int64MulOp                  -> "Int64Mul"
  Int64NegOp                  -> "Int64Negate"
  Int64SllOp                  -> "Int64Shl"
  Int64SraOp                  -> "Int64Shra"
  Int64LtOp                   -> "Int64Lt"
  Int64LeOp                   -> "Int64Le"
  Int64GtOp                   -> "Int64Gt"
  Int64GeOp                   -> "Int64Ge"
  Int64ToIntOp                -> "Int64ToInt"
  IntToInt64Op                -> "IntToInt64"
  Int64ToWord64Op             -> "Int64ToWord64"
  Int64QuotOp                 -> "Int64Quot"
  Int64RemOp                  -> "Int64Rem"
  Int64SrlOp                  -> "Int64Shrl"
  Int64EqOp                   -> "Int64Eq"
  Int64NeOp                   -> "Int64Ne"
  -- Word64
  Word64ToInt64Op             -> "Word64ToInt64"
  Word64ToWordOp              -> "Word64ToWord"
  WordToWord64Op              -> "WordToWord64"
  Word64SllOp                 -> "Word64Shl"
  Word64SrlOp                 -> "Word64Shrl"
  Word64OrOp                  -> "Word64Or"
  Word64AndOp                 -> "Word64And"
  Word64EqOp                  -> "Word64Eq"
  Word64NeOp                  -> "Word64Ne"
  Word64LtOp                  -> "Word64Lt"
  Word64LeOp                  -> "Word64Le"
  Word64GtOp                  -> "Word64Gt"
  Word64GeOp                  -> "Word64Ge"
  Word64AddOp                 -> "Word64Add"
  Word64SubOp                 -> "Word64Sub"
  Word64MulOp                 -> "Word64Mul"
  Word64QuotOp                -> "Word64Quot"
  Word64RemOp                 -> "Word64Rem"
  Word64XorOp                 -> "Word64Xor"
  Word64NotOp                 -> "Word64Not"
  -- Carry arithmetic and wide multiply handled by splitMultiReturnPrimOp / splitTripleReturnPrimOp
  -- CLZ
  Clz8Op                      -> "Clz8"
  ClzOp                       -> "Clz"
  -- SmallArray#
  NewSmallArrayOp             -> "NewSmallArray"
  ReadSmallArrayOp            -> "ReadSmallArray"
  WriteSmallArrayOp           -> "WriteSmallArray"
  IndexSmallArrayOp           -> "IndexSmallArray"
  SizeofSmallArrayOp          -> "SizeofSmallArray"
  SizeofSmallMutableArrayOp   -> "SizeofSmallMutableArray"
  GetSizeofSmallMutableArrayOp -> "SizeofSmallMutableArray"
  UnsafeFreezeSmallArrayOp    -> "UnsafeFreezeSmallArray"
  UnsafeThawSmallArrayOp      -> "UnsafeThawSmallArray"
  CopySmallArrayOp            -> "CopySmallArray"
  CopySmallMutableArrayOp     -> "CopySmallMutableArray"
  CloneSmallArrayOp           -> "CloneSmallArray"
  CloneSmallMutableArrayOp    -> "CloneSmallMutableArray"
  ShrinkSmallMutableArrayOp_Char -> "ShrinkSmallMutableArray"
  CasSmallArrayOp             -> "CasSmallArray"
  -- Array#
  NewArrayOp                  -> "NewArray"
  ReadArrayOp                 -> "ReadArray"
  WriteArrayOp                -> "WriteArray"
  SizeofArrayOp               -> "SizeofArray"
  SizeofMutableArrayOp        -> "SizeofMutableArray"
  UnsafeFreezeArrayOp         -> "UnsafeFreezeArray"
  UnsafeThawArrayOp           -> "UnsafeThawArray"
  CopyArrayOp                 -> "CopyArray"
  CopyMutableArrayOp          -> "CopyMutableArray"
  CloneArrayOp                -> "CloneArray"
  CloneMutableArrayOp         -> "CloneMutableArray"
  -- Bit operations
  PopCntOp                    -> "PopCnt"
  PopCnt8Op                   -> "PopCnt8"
  PopCnt16Op                  -> "PopCnt16"
  PopCnt32Op                  -> "PopCnt32"
  PopCnt64Op                  -> "PopCnt64"
  CtzOp                       -> "Ctz"
  Ctz8Op                      -> "Ctz8"
  Ctz16Op                     -> "Ctz16"
  Ctz32Op                     -> "Ctz32"
  Ctz64Op                     -> "Ctz64"
  -- Exception
  RaiseOp     -> "Raise"
  -- Arithmetic exceptions raised in ghc-bignum's check branches.
  RaiseUnderflowOp -> "RaiseUnderflow"
  RaiseOverflowOp  -> "RaiseOverflow"
  RaiseDivZeroOp   -> "RaiseDivZero"
  other       -> error $ "Unsupported primop: " ++ showPprUnsafe other

-- | Float transcendental primops (expFloat#, sinFloat#, …) have no hardware
-- opcode. Map each to its Double-precision sibling's serial name so it can be
-- desugared to the Double libm path (the JIT/eval implement only Double libm).
-- Hardware-opcode Float ops (sqrtFloat#/fabsFloat#) are NOT here — they go
-- through mapPrimOp natively. Returns Nothing for any non-transcendental primop.
floatMathToDouble :: PrimOp -> Maybe Text
floatMathToDouble = \case
  FloatExpOp   -> Just "DoubleExp"
  FloatExpM1Op -> Just "DoubleExpM1"
  FloatLogOp   -> Just "DoubleLog"
  FloatLog1POp -> Just "DoubleLog1P"
  FloatSinOp   -> Just "DoubleSin"
  FloatCosOp   -> Just "DoubleCos"
  FloatTanOp   -> Just "DoubleTan"
  FloatAsinOp  -> Just "DoubleAsin"
  FloatAcosOp  -> Just "DoubleAcos"
  FloatAtanOp  -> Just "DoubleAtan"
  FloatSinhOp  -> Just "DoubleSinh"
  FloatCoshOp  -> Just "DoubleCosh"
  FloatTanhOp  -> Just "DoubleTanh"
  FloatAsinhOp -> Just "DoubleAsinh"
  FloatAcoshOp -> Just "DoubleAcosh"
  FloatAtanhOp -> Just "DoubleAtanh"
  FloatPowerOp -> Just "DoublePower"
  _            -> Nothing

-- | Recognize primops that return unboxed tuples and can be split into
-- two individual primops. Returns (primop1, primop2) text names.
splitMultiReturnPrimOp :: PrimOp -> Maybe (Text, Text)
splitMultiReturnPrimOp = \case
  IntQuotRemOp  -> Just (T.pack "IntQuot", T.pack "IntRem")
  WordQuotRemOp -> Just (T.pack "WordQuot", T.pack "WordRem")
  Word8QuotRemOp -> Just (T.pack "Word8Quot", T.pack "Word8Rem")
  IntAddCOp     -> Just (T.pack "AddIntCVal", T.pack "AddIntCCarry")
  IntSubCOp     -> Just (T.pack "SubIntCVal", T.pack "SubIntCCarry")
  WordAddCOp    -> Just (T.pack "AddWordCVal", T.pack "AddWordCCarry")
  WordSubCOp    -> Just (T.pack "SubWordCVal", T.pack "SubWordCCarry")
  WordMul2Op    -> Just (T.pack "TimesWord2Hi", T.pack "TimesWord2Lo")
  WordAdd2Op    -> Just (T.pack "WordAdd2Hi", T.pack "WordAdd2Lo")
  _             -> Nothing

-- | 3-input / 2-output primops: @quotRemWord2# high low divisor -> (# q, r #)@.
-- Like 'splitMultiReturnPrimOp' but the scrutinee op takes THREE value args.
splitWord2DivPrimOp :: PrimOp -> Maybe (Text, Text)
splitWord2DivPrimOp = \case
  WordQuotRem2Op -> Just (T.pack "WordQuotRem2Quot", T.pack "WordQuotRem2Rem")
  _              -> Nothing

-- | Like splitMultiReturnPrimOp but for primops returning 3-element unboxed tuples.
splitTripleReturnPrimOp :: PrimOp -> Maybe (Text, Text, Text)
splitTripleReturnPrimOp = \case
  -- timesInt2# returns (# isHighNeeded#, high#, low# #) — the FIRST component is
  -- the overflow flag, not the high word. The native ghc-bignum backend's
  -- integerMul small path relies on this exact order (it was dormant under the
  -- gmp backend, which multiplied via FFI).
  IntMul2Op -> Just (T.pack "TimesInt2Overflow", T.pack "TimesInt2Hi", T.pack "TimesInt2Lo")
  _         -> Nothing

-- | Like splitMultiReturnPrimOp but for unary primops (single argument)
-- returning unboxed tuples.
splitUnaryMultiReturnPrimOp :: PrimOp -> Maybe (Text, Text)
splitUnaryMultiReturnPrimOp = \case
  DoubleDecode_Int64Op -> Just (T.pack "DecodeDoubleMantissa", T.pack "DecodeDoubleExponent")
  FloatDecode_IntOp    -> Just (T.pack "DecodeFloatMantissa", T.pack "DecodeFloatExponent")
  _                    -> Nothing

primOpArity :: PrimOp -> Int
primOpArity op = let (_, _, _, a, _) = primOpSig op in a
