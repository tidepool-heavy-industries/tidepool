{-# LANGUAGE NoImplicitPrelude #-}

-- | Runtime helpers for spec'd @[fmt|{expr:spec}|]@ holes.
--
-- == Backend strategy (B)
--
-- PyF's own formatting machinery (@PyF.Formatters@) renders floats through
-- @Numeric.show{F,E,G}Float@ → @floatToDigits@ → the @clz#@ primop, which the
-- Cranelift JIT does not implement (verified: every PyF float hole traps).  So
-- the quoter ("Tidepool.QQ.Fmt") interprets the parsed 'FormatMode' at
-- /compile time/ and emits a call to one of the monomorphic, JIT-safe helpers
-- below.  Floats are rendered with the @round@ primop (now lowered to
-- Cranelift @nearest@) plus 'Int' arithmetic; no @floatToDigits@ anywhere.
--
-- == Why everything runs in 'String' space
--
-- Assembly (padding, sign, digit grouping) happens on @[Char]@ and the result
-- is wrapped with a single @T.pack@ at the boundary (Text inputs are
-- @T.unpack@'d on the way in).  This is the proven @T.pack (go (T.unpack t))@
-- shape used elsewhere in the suite; it avoids @T.concat@ / @T.length@ over
-- intermediate (possibly empty) 'Text' values, which the tree-walking
-- interpreter cannot read.
--
-- Kept lens-free (like "Tidepool.Render") so the @--all-closed@ extract
-- session can load it; "Tidepool.Prelude" re-exports everything here.
module Tidepool.QQ.Fmt.Runtime
  ( FSign (..)
  , FAlign (..)
  , fmtInt
  , fmtFrac
  , fmtStr
  , fmtChar
  , fmtSigned
  , fmtPlain
  ) where

import Data.Text (Text)
import qualified Data.Text as T
import Data.Char (chr)
import Prelude
  ( Int, Double, Char, Bool (..), Maybe (..), String
  , (+), (-), (*), negate, div, mod, quotRem, round, show
  , (<), (<=), (==), (>), (&&), otherwise, replicate, length, (++), (!!), reverse, take
  )

-- | Sign mode, mirrored from "Tidepool.QQ.PyF.Spec".'SignMode' as a runtime
-- value the quoter constructs (so call sites must have these in scope —
-- "Tidepool.Prelude" re-exports them).
data FSign = FPlus | FMinus | FSpace

-- | Alignment, mirrored from "Tidepool.QQ.PyF.Spec".'Align'.
data FAlign = FLeft | FRight | FCenter | FInside

------------------------------------------------------------------------
-- Public helpers (one per presentation-type category)
------------------------------------------------------------------------

-- | Integral types: decimal (@d@), hex (@x@\/@X@), octal (@o@), binary (@b@).
-- @base@ is 10\/16\/8\/2; @upper@ uppercases hex; @alt@ adds the @0x@\/@0o@\/
-- @0b@ prefix; @grp@ inserts a grouping char every three digits.
fmtInt :: FSign -> Int -> Bool -> Bool -> Maybe Char -> Int -> Char -> FAlign -> Int -> Text
fmtInt sgn base upper alt grp width fill align n =
  let neg   = n < 0
      mag   = if neg then negate n else n
      digs0 = digitsInBase base upper mag
      digs  = case grp of
        Nothing -> digs0
        Just c  -> groupDigits c digs0
      prefix = if alt then basePrefix base upper else ""
      pre    = (if neg then "-" else signStr sgn) ++ prefix
   in T.pack (fpad align width fill pre digs)

-- | Fractional types: fixed-point (@f@\/@F@) and percent (@%@), via the
-- @round@ primop.  @percent@ pre-multiplies by 100 and appends @%@; @prec@ is
-- the number of fractional digits.
fmtFrac :: FSign -> Bool -> Int -> Int -> Char -> FAlign -> Double -> Text
fmtFrac sgn percent prec width fill align d0 =
  let d      = if percent then d0 * 100.0 else d0
      neg    = d < 0.0
      a      = if neg then negate d else d
      scaled = round (a * powD prec) :: Int
      pI     = powI prec
      ip     = scaled `div` pI
      fp     = scaled `mod` pI
      fracS  = if prec <= 0 then "" else "." ++ padZeros prec (show fp)
      body   = show ip ++ fracS ++ (if percent then "%" else "")
      pre    = if neg then "-" else signStr sgn
   in T.pack (fpad align width fill pre body)

-- | The string type (@s@): optional precision truncates to @Just maxLen@.
fmtStr :: Maybe Int -> Int -> Char -> FAlign -> Text -> Text
fmtStr mprec width fill align t =
  let s  = T.unpack t
      s' = case mprec of
        Nothing -> s
        Just p  -> take p s
   in T.pack (fpad align width fill "" s')

-- | The character type (@c@): interpret the 'Int' as a code point.
fmtChar :: Int -> Char -> FAlign -> Int -> Text
fmtChar width fill align n = T.pack (fpad align width fill "" [chr n])

-- | Type-less hole carrying an explicit @+@\/space sign: the value is already
-- rendered to 'Text', so the sign of the number is recovered from a leading
-- @-@.
fmtSigned :: FSign -> Int -> Char -> FAlign -> Text -> Text
fmtSigned sgn width fill align t =
  let (neg, mag) = case T.unpack t of
        ('-' : rest) -> (True, rest)
        s            -> (False, s)
      pre = if neg then "-" else signStr sgn
   in T.pack (fpad align width fill pre mag)

-- | Type-less hole with no explicit sign: just lay the rendered 'Text' out to
-- the requested width.  (A leading @-@ from @render@ is preserved untouched.)
fmtPlain :: Int -> Char -> FAlign -> Text -> Text
fmtPlain width fill align t = T.pack (fpad align width fill "" (T.unpack t))

------------------------------------------------------------------------
-- Shared internals (all in String space)
------------------------------------------------------------------------

-- | Lay @prefix@ (sign + base-prefix) and @body@ (digits) out to @width@,
-- distributing the fill per alignment.  Mirrors PyF's @padAndSign@.
fpad :: FAlign -> Int -> Char -> String -> String -> String
fpad align width fill pre body =
  let len  = length pre + length body
      need = let n = width - len in if n < 0 then 0 else n
      pad k = replicate k fill
   in case align of
        FLeft   -> pre ++ body ++ pad need
        FRight  -> pad need ++ pre ++ body
        FCenter -> let l = need `div` 2 in pad l ++ pre ++ body ++ pad (need - l)
        FInside -> pre ++ pad need ++ body

signStr :: FSign -> String
signStr FPlus  = "+"
signStr FSpace = " "
signStr FMinus = ""

basePrefix :: Int -> Bool -> String
basePrefix 16 upper = if upper then "0X" else "0x"
basePrefix 8  _     = "0o"
basePrefix 2  _     = "0b"
basePrefix _  _     = ""

-- | Digits of a non-negative 'Int' in the given base, most significant first.
digitsInBase :: Int -> Bool -> Int -> String
digitsInBase base upper = go []
  where
    tbl = if upper then "0123456789ABCDEF" else "0123456789abcdef"
    go acc x =
      let (q, r) = x `quotRem` base
          acc'   = (tbl !! r) : acc
       in if q == 0 then acc' else go acc' q

-- | Left-pad a fractional digit string with zeros to @n@ characters.
padZeros :: Int -> String -> String
padZeros n s = replicate (n - length s) '0' ++ s

-- | Insert @c@ between every group of three digits (from the right).
groupDigits :: Char -> String -> String
groupDigits c s = reverse (go (0 :: Int) (reverse s))
  where
    go _ [] = []
    go k (x : xs)
      | k > 0 && k `mod` 3 == 0 = c : x : go (k + 1) xs
      | otherwise               = x : go (k + 1) xs

-- | @10 ^ n@ as a 'Double' via repeated multiplication (avoids @fromIntegral@,
-- which routes through 'Integer').
powD :: Int -> Double
powD = go 1.0
  where
    go acc k = if k <= 0 then acc else go (acc * 10.0) (k - 1)

-- | @10 ^ n@ as an 'Int'.
powI :: Int -> Int
powI = go 1
  where
    go acc k = if k <= 0 then acc else go (acc * 10) (k - 1)
