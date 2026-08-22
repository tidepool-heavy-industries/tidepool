{-# LANGUAGE ScopedTypeVariables #-}

-- | Generates @haskell\/test\/corpus_cbor\/oracle.json@: the GHC-computed
-- expected value for every scalar\/list-of-scalar binding in "Corpus", run
-- alongside @regen-corpus.sh@'s CBOR fixture regeneration so both artifacts
-- come from the ONE regen entry point.
--
-- Wire format (consumed by @tidepool-codegen\/tests\/real_core_corpus.rs@ —
-- see that file's module doc for the Rust-side decode contract this must
-- satisfy): one JSON object mapping each corpus binding's name to
-- @{"mode","kind","value"}@.
--
-- @mode@ is always @"NF"@ here: every "Corpus" binding is a fully-evaluable
-- scalar or list of scalars with no field that must stay unforced surviving
-- INTO the compared value itself (a case like 'Corpus.lazyConField' hides an
-- unforced bottom inside a tuple that is never part of the returned `Int`,
-- so forcing the `Int` result to normal form never touches it). This matches
-- what @JitEffectMachine::run_pure@'s heap bridge already forces the JIT side
-- to, which is what this sidecar must agree with. @"WHNF"@\/@"Display"@ are
-- reserved by the schema for a future binding that genuinely needs a
-- shallower or textual observation; none of today's corpus does.
--
-- @kind@ tells the Rust-side decoder which GHC boxed representation to
-- expect and how to interpret @value@:
--
-- > int      -- Int/Word/Int8/16/32/64/Word8/16/32/64 (JSON string, decimal)
-- > integer  -- arbitrary-precision Integer, IS/IP/IN  (JSON string, decimal)
-- > double   -- Double                                  (JSON number)
-- > float    -- Float, widened to Double for comparison  (JSON number)
-- > bool     -- Bool                                     (JSON true/false)
-- > char     -- Char                                     (JSON 1-char string)
-- > string   -- String ([Char])                          (JSON string)
-- > list_int -- [Int]                                    (JSON array of numbers)
--
-- No aeson dependency: this executable hand-rolls the tiny amount of JSON
-- needed for this fixed, flat schema (see 'jsonEscape') rather than pull in
-- a new Hackage dependency for one generator.
module Main (main) where

import Corpus
import Data.Char (ord)
import Data.List (intercalate)
import System.Environment (getArgs)
import System.Exit (exitFailure)
import System.IO (hPutStrLn, stderr)
import Numeric (showHex)

quote :: String -> String
quote s = "\"" ++ s ++ "\""

-- | Escape a Haskell 'String' into a JSON string body (without the
-- surrounding quotes). Every corpus 'Corpus.String'/'Corpus.Char' result is
-- plain ASCII today; the control-char escape is still applied generally so a
-- future non-ASCII entry fails to produce invalid JSON rather than silently
-- emitting a raw control byte.
jsonEscape :: String -> String
jsonEscape = concatMap esc
  where
    esc '"' = "\\\""
    esc '\\' = "\\\\"
    esc '\n' = "\\n"
    esc '\r' = "\\r"
    esc '\t' = "\\t"
    esc c
      | ord c < 0x20 = "\\u" ++ pad (showHex (ord c) "")
      | otherwise = [c]
    pad h = replicate (4 - length h) '0' ++ h

field :: String -> String -> String -> String
field name kind valueJson =
  quote name ++ ":{\"mode\":\"NF\",\"kind\":" ++ quote kind ++ ",\"value\":" ++ valueJson ++ "}"

-- | Any fixed-width integral (Int/Word/Int8/16/32/64/Word8/16/32/64):
-- rendered as a decimal-digit JSON STRING (not a JSON number) so the Rust
-- side never has to reason about JSON-number precision — it parses the
-- digits itself once it knows, from `kind`, which boxed GHC constructor
-- family to expect.
entryInt :: Integral a => String -> a -> String
entryInt name n = field name "int" (quote (show (toInteger n)))

-- | Arbitrary-precision 'Integer' (GHC's IS\/IP\/IN boxed representation):
-- same decimal-string wire shape as 'entryInt', distinguished by `kind` so
-- the Rust decoder looks for the right constructor family.
entryInteger :: String -> Integer -> String
entryInteger name n = field name "integer" (quote (show n))

entryBool :: String -> Bool -> String
entryBool name b = field name "bool" (if b then "true" else "false")

entryChar :: String -> Char -> String
entryChar name c = field name "char" (quote (jsonEscape [c]))

entryString :: String -> String -> String
entryString name s = field name "string" (quote (jsonEscape s))

entryListInt :: Integral a => String -> [a] -> String
entryListInt name xs =
  field name "list_int" ("[" ++ intercalate "," (map (show . toInteger) xs) ++ "]")

-- | GHC's 'Show' instance for 'Double' is a shortest-round-trip decimal
-- encoder, so its output is both valid JSON number syntax (digits, optional
-- fractional part, optional signed exponent) and parses back on the Rust
-- side (serde_json's f64 parser is also correctly-rounded) to the identical
-- bit pattern. NaN\/Infinity have no JSON encoding; none of today's corpus
-- produces one, so this fails loudly rather than emitting invalid JSON.
showJsonDouble :: Double -> String
showJsonDouble x
  | isNaN x || isInfinite x =
      error ("corpus oracle: non-finite Double has no JSON encoding: " ++ show x)
  | otherwise = show x

entryDouble :: String -> Double -> String
entryDouble name x = field name "double" (showJsonDouble x)

-- | 'Float' -> 'Double' widening is exact (f32 is a subset of f64
-- precision), so promoting before rendering loses nothing; the Rust side
-- promotes its own unboxed F# reading the same way before comparing.
entryFloat :: String -> Float -> String
entryFloat name x = field name "float" (showJsonDouble (realToFrac x))

-- | One entry per top-level 0-arity "Corpus" binding with a concrete
-- scalar\/list-of-scalar result type — i.e. every binding the real-Core
-- corpus differential (`real_core_corpus.rs`) actually replays as a
-- "program" (helper functions like `buildTree`/`sumTree`/`retC`/`bindC`/
-- `evalE` evaluate to a `Closure` there and are skipped, so they have no
-- entry here either). Generated once from `Corpus.hs`'s own type
-- signatures (see the commit that introduced this file) and then
-- hand-maintained: add one line here whenever a new corpus binding is added.
entries :: [String]
entries =
  [ entryInteger "seedI5" seedI5,
    entryInteger "seedI1025" seedI1025,
    entryString "seed42" seed42,
    entryString "seedPi" seedPi,
    entryString "seedList" seedList,
    entryDouble "seedD" seedD,
    entryDouble "convFromInt5" convFromInt5,
    entryDouble "convFromInt1025" convFromInt1025,
    entryDouble "convFromIntPow40" convFromIntPow40,
    entryDouble "convFromIntPow80" convFromIntPow80,
    entryDouble "convFromRational" convFromRational,
    entryDouble "convDoubleLitBig" convDoubleLitBig,
    entryDouble "convRealToFrac" convRealToFrac,
    entryInteger "convFloor" convFloor,
    entryInteger "convCeiling" convCeiling,
    entryInteger "convRound" convRound,
    entryInteger "convTruncate" convTruncate,
    entryInteger "convProperFraction" convProperFraction,
    entryInt "readInt" readInt,
    entryDouble "readDouble" readDouble,
    entryListInt "readListInt" readListInt,
    entryInt "newtypeFn" newtypeFn,
    entryInt "recordClosures" recordClosures,
    entryInt "contMonad" contMonad,
    entryInt "sumRange" sumRange,
    entryInt "foldlSum" foldlSum,
    entryInt "manualLoop" manualLoop,
    entryBool "mutualEven" mutualEven,
    entryString "showInt" showInt,
    entryString "showListInt" showListInt,
    entryInt "customClass" customClass,
    entryInt "gadtEval" gadtEval,
    entryInt "enumRoundTrip" enumRoundTrip,
    entryListInt "takeWhileList" takeWhileList,
    entryListInt "nubList" nubList,
    entryListInt "sortList" sortList,
    entryDouble "sqrtD" sqrtD,
    entryString "showDoubleB" showDoubleB,
    entryListInt "cycleTake" cycleTake,
    entryListInt "zipWithIdx" zipWithIdx,
    entryListInt "mapMaybeEven" mapMaybeEven,
    entryInt "maybeChain" maybeChain,
    entryInt "eitherChain" eitherChain,
    entryInt "intArith" intArith,
    entryString "integerShow" integerShow,
    entryInteger "integerProduct" integerProduct,
    entryInt "gcdLcm" gcdLcm,
    entryInt "wordArith" wordArith,
    entryInt "negAbs" negAbs,
    entryDouble "floatArith" floatArith,
    entryInt "strictPair" strictPair,
    entryInt "treeSum" treeSum,
    entryBool "ordCompare" ordCompare,
    entryInt "nestedMaybe" nestedMaybe,
    entryInt "charClass" charClass,
    entryListInt "scanlAccum" scanlAccum,
    entryListInt "concatMapList" concatMapList,
    entryString "unwordsJoin" unwordsJoin,
    entryDouble "floatingSin" floatingSin,
    entryDouble "floatingExpLog" floatingExpLog,
    entryDouble "floatingTanAtan" floatingTanAtan,
    entryDouble "floatingPow" floatingPow,
    entryFloat "floatVal" floatVal,
    entryBool "floatCompare" floatCompare,
    entryInt "narrowWord8" narrowWord8,
    entryInt "narrowInt8" narrowInt8,
    entryInt "seqForce" seqForce,
    entryInt "floorOfSin" floorOfSin,
    entryInt "lazyConField" lazyConField,
    entryInt "bangStrict" bangStrict,
    entryInt "seqChain" seqChain,
    entryListInt "takeInfinite" takeInfinite,
    entryListInt "fibsTake" fibsTake,
    entryListInt "zipInfinite" zipInfinite,
    entryListInt "repeatTake" repeatTake,
    entryListInt "iterateTake" iterateTake,
    entryInt "sharedThunk" sharedThunk,
    entryInt "gcMapSum" gcMapSum,
    entryInt "gcConcatLen" gcConcatLen,
    entryInt "gcFilterLen" gcFilterLen,
    entryInt "gcReverseSum" gcReverseSum,
    entryInt "gcTreeBuild" gcTreeBuild,
    entryInt "gcStringAlloc" gcStringAlloc,
    entryFloat "sF" sF,
    entryDouble "sD" sD,
    entryChar "sC" sC,
    entryInt "sW" sW,
    entryInt "sI" sI,
    entryInt "sI64" sI64,
    entryFloat "pcFloatArith" pcFloatArith,
    entryFloat "pcFloatNeg" pcFloatNeg,
    entryBool "pcFloatCmp" pcFloatCmp,
    entryFloat "pcFloatSqrt" pcFloatSqrt,
    entryFloat "pcFloatTranscend" pcFloatTranscend,
    entryDouble "pcDoubleSubMul" pcDoubleSubMul,
    entryBool "pcDoubleCmp" pcDoubleCmp,
    entryBool "pcCharCmp" pcCharCmp,
    entryInt "pcIntNot" pcIntNot,
    entryInt "pcWordXor" pcWordXor,
    entryDouble "pcTranscend" pcTranscend,
    entryDouble "pcTranscend2" pcTranscend2,
    entryFloat "pcIntToFloat" pcIntToFloat,
    entryInt "pcFloatToInt" pcFloatToInt,
    entryFloat "pcDoubleToFloat" pcDoubleToFloat,
    entryInt "pcNarrowW8" pcNarrowW8,
    entryInt "pcNarrowI8" pcNarrowI8,
    entryInt "pcNarrowW16" pcNarrowW16,
    entryInt "pcNarrowI16" pcNarrowI16,
    entryInt "pcNarrowW32" pcNarrowW32,
    entryInt "pcInt64Arith" pcInt64Arith,
    entryBool "pcInt64Cmp" pcInt64Cmp,
    entryInt "sW8" sW8,
    entryInt "pcWordMul" pcWordMul,
    entryBool "pcWordCmp" pcWordCmp,
    entryDouble "pcDoubleFabs" pcDoubleFabs,
    entryInt "pcWord8Arith" pcWord8Arith,
    entryBool "pcWord8Cmp" pcWord8Cmp,
    entryInt "pcQuotRemW" pcQuotRemW,
    entryInt "pcDivModInt" pcDivModInt
  ]

main :: IO ()
main = do
  args <- getArgs
  case args of
    [outPath] -> writeFile outPath ("{\n" ++ intercalate ",\n" (map ("  " ++) entries) ++ "\n}\n")
    _ -> do
      hPutStrLn stderr "usage: corpus-oracle-gen <output-path>"
      exitFailure
