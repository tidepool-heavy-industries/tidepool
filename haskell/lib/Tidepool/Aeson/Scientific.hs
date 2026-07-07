{-# LANGUAGE ScopedTypeVariables #-}
-- | Vendored, minimal @Data.Scientific@ — an arbitrary-precision decimal
-- @coefficient * 10 ^ base10Exponent@, faithful enough for JSON numbers.
--
-- This is the aeson number carrier: @'Tidepool.Aeson.Value.Value'@ wraps a
-- single 'Scientific' in its @Number@ constructor, so integer and fractional
-- JSON numbers share one exact representation (no @Double@ round-trip, no
-- Int64 cap). Integer arithmetic runs on the JIT via @tidepool-bignum@, so the
-- coefficient may be an arbitrary 'Integer'.
--
-- Kept opaque (no record labels) so the heap/render shape stays a plain
-- arity-2 constructor. This module must NOT import "Tidepool.Aeson.Value"
-- (Value imports Scientific, not the reverse).
--
-- Differences from upstream @scientific@:
--   - 'fromFloatDigits' recovers the shortest round-trip decimal from
--     @show \@Double@ (lowered to Rust's shortest-float formatting by the
--     extractor) and parses it, rather than @Numeric.floatToDigits@, which the
--     JIT cannot run (it hits the @clz#@ primop). Same shortest-decimal upstream
--     produces.
--   - 'Show' renders a canonical decimal that MIRRORS the Rust JSON renderer
--     (full-digit integers, fixed-point fractions, NEVER exponent form), so it
--     agrees with @renderJson@ and the eval-result render — unlike upstream's
--     @show@, which switches to scientific notation outside a fixed window.
--   - No 'Fractional'/'RealFrac' instances: JSON numbers never need division,
--     and the eval surface reaches integer parts of a number through 'toBoundedInteger'
--     / the @_Int@ prism, not @round@/@truncate@ on a 'Scientific'.
module Tidepool.Aeson.Scientific
  ( Scientific(..)
  , scientific
  , coefficient
  , base10Exponent
  , fromFloatDigits
  , toRealFloat
  , toBoundedInteger
  , truncateScientific
  , floorScientific
  , floorBoundedInteger
  , floatingOrInteger
  , isFiniteDouble
  ) where

import Prelude
import Data.Char (chr, ord)
import Data.List (foldl')
import Data.Ratio ((%))

-- | An arbitrary-precision decimal: @Scientific c e@ denotes @c * 10 ^ e@.
-- Opaque, single constructor, arity 2 (coefficient first, exponent second) —
-- no record labels, to keep the heap shape a bare @Con(Scientific,[c,e])@.
data Scientific = Scientific !Integer !Int

-- | Smart constructor for @coefficient * 10 ^ base10Exponent@. Normalizes by
-- stripping trailing-zero coefficient digits (raising the exponent), which
-- canonicalizes the representation without changing the denoted value. Total
-- (zero is handled specially so the strip loop terminates).
scientific :: Integer -> Int -> Scientific
scientific c e = let (c', e') = stripZeros c e in Scientific c' e'

-- | The coefficient (may be any 'Integer', positive or negative).
coefficient :: Scientific -> Integer
coefficient (Scientific c _) = c

-- | The base-10 exponent.
base10Exponent :: Scientific -> Int
base10Exponent (Scientific _ e) = e

-- Strip trailing zeros from the coefficient, raising the exponent to match.
-- @quotRem@ truncates toward zero, so the coefficient sign is preserved.
stripZeros :: Integer -> Int -> (Integer, Int)
stripZeros 0 _ = (0, 0)
stripZeros c e = case c `quotRem` 10 of
  (q, 0) -> stripZeros q (e + 1)
  _      -> (c, e)

-- Numeric value as an exact 'Rational' (@c * 10 ^^ e@).
instance Real Scientific where
  toRational (Scientific c e)
    | e >= 0    = (c * pow10 e) % 1
    | otherwise = c % pow10 (negate e)

-- Equality and ordering are BY NUMERIC VALUE, not structural: @Scientific 1 1@
-- equals @Scientific 10 0@ (both denote 10). Routed through 'toRational'.
instance Eq Scientific where
  a == b = toRational a == toRational b

instance Ord Scientific where
  compare a b = compare (toRational a) (toRational b)

instance Num Scientific where
  Scientific c1 e1 + Scientific c2 e2 =
    let e = min e1 e2
    in scientific (c1 * pow10 (e1 - e) + c2 * pow10 (e2 - e)) e
  Scientific c1 e1 * Scientific c2 e2 = scientific (c1 * c2) (e1 + e2)
  negate (Scientific c e) = Scientific (negate c) e
  abs    (Scientific c e) = Scientific (abs c) e
  signum (Scientific c _) = scientific (signum c) 0
  fromInteger n           = scientific n 0

-- Canonical decimal rendering that MIRRORS the Rust JSON renderer
-- (@render.rs@ @compose_decimal@): full-digit integers (no @.0@, no exponent
-- form) and plain fixed-point fractions. So @show@, the user-facing @renderJson@
-- verb (which is @show@ on a Number), and the eval-result render all agree on
-- ONE form — an exact big integer shows as its full digits, never @9.007e15@,
-- keeping the exact-Integer guarantee visible through every path.
instance Show Scientific where
  show (Scientific c e)
    | c < 0     = '-' : showPos (negate c) e
    | otherwise = showPos c e

-- Render a non-negative @c * 10 ^ e@ as a canonical decimal (never exponent
-- form). @stripZeros@ first drops trailing coefficient zeros, so a fractional
-- part never has trailing zeros — matching @compose_decimal@'s trim.
showPos :: Integer -> Int -> String
showPos c e =
  let (c', e') = stripZeros c e
      ds       = map intToDigitC (digitsOf c')   -- most-significant first
      m        = length ds
  in if e' >= 0
       then ds ++ replicate e' '0'               -- integer: bare digits, no ".0"
       else let k = negate e'                     -- number of fractional digits
            in if k < m
                 then let (intPart, fracPart) = splitAt (m - k) ds
                      in intPart ++ '.' : fracPart
                 else "0." ++ replicate (k - m) '0' ++ ds

-- Decimal digits of a non-negative 'Integer', most-significant first ([0] for 0).
digitsOf :: Integer -> [Int]
digitsOf n
  | n < 0     = digitsOf (negate n)
  | n < 10    = [fromInteger n]
  | otherwise = digitsOf (n `div` 10) ++ [fromInteger (n `mod` 10)]

intToDigitC :: Int -> Char
intToDigitC d = chr (48 + d)

-- | Convert a 'RealFloat' (e.g. 'Double') to a 'Scientific' carrying the
-- SHORTEST decimal that round-trips the float, via @show \@Double@ (a primop)
-- (@Numeric.floatToDigits@ can't run on the JIT — it hits @clz#@). So
-- @fromFloatDigits 3.14@ is @scientific 314 (-2)@, not the exact 52-digit
-- binary expansion — which is what makes @toJSON (3.14 :: Double)@ render as
-- @3.14@ rather than @3.1400000000000001…@. Round-trips exactly through
-- 'toRealFloat'. (An extreme whole-number magnitude like @1.79e308@ still
-- renders expanded — a small coefficient with a large positive exponent is an
-- integer; compacting those to exponent form would need double-vs-integer
-- origin tracking the erased 'Value' representation can't carry.)
fromFloatDigits :: RealFloat a => a -> Scientific
fromFloatDigits = fromDouble . realToFrac

-- | Is the 'Double' neither NaN nor +/-Infinity? 'fromDouble' parses the
-- LETTERS of @show \@Double@'s "NaN"\/"Infinity" spellings as decimal digits
-- (there is no numeric decimal form to parse), so callers that may see a
-- non-finite value — the @'Tidepool.Aeson.Value.ToJSON' 'Double'@\/'Float'
-- instances — must guard with this BEFORE calling 'fromFloatDigits'\/'fromDouble'
-- and map to @Null@ instead (matching upstream aeson's non-finite encoding).
-- Tested WITHOUT a 'RealFloat' dictionary (mirrors
-- "Tidepool.QQ.Fmt.Runtime".'Tidepool.QQ.Fmt.Runtime.fmtFrac''s guard, which
-- has the same JIT-safety motive): @d \/= d@ iff NaN; @|d|@ is its own fixed
-- point under halving iff +\/-Infinity among nonzero finites.
isFiniteDouble :: Double -> Bool
isFiniteDouble d = d == d && not (absD > 0.0 && absD * 0.5 == absD)
  where absD = if d < 0 then negate d else d

-- Monomorphic core. Uses @show \@Double@ — which the extractor lowers to the
-- @ShowDoubleAddr@ primop (shortest-decimal, JIT-safe) — rather than a local
-- @showDouble@ stub, whose bottoming body GHC can collapse past the interceptor.
-- PRECONDITION: @d@ is finite ('isFiniteDouble' d) — callers that may see a
-- non-finite value must check first (see 'isFiniteDouble').
fromDouble :: Double -> Scientific
fromDouble = readDecimalToScientific . show

-- Parse a decimal string — fixed (@\"3.14\"@) or scientific (@\"1.79e308\"@),
-- optional leading @-@ — into an exact 'Scientific'. Inputs come from the
-- @show \@Double@ primop, so they are always well-formed; no error handling.
readDecimalToScientific :: String -> Scientific
readDecimalToScientific s0 =
  let (neg, s1) = case s0 of
        ('-':r) -> (True, r)
        _       -> (False, s0)
      (mant, expPart) = break (\c -> c == 'e' || c == 'E') s1
      e0 = case expPart of
             (_:r) -> readInt r          -- drop the leading 'e'/'E'
             _     -> 0
      (intPart, fracPart) = break (== '.') mant
      frac = case fracPart of
               (_:r) -> r                -- drop the leading '.'
               _     -> ""
      coeff0 = readDigits (intPart ++ frac)
      coeff  = if neg then negate coeff0 else coeff0
  in scientific coeff (e0 - length frac)

-- Read a run of decimal digits as a non-negative 'Integer'.
readDigits :: String -> Integer
readDigits = foldl' (\acc c -> acc * 10 + toInteger (ord c - 48)) 0

-- Read a possibly sign-prefixed 'Int' exponent.
readInt :: String -> Int
readInt ('-':r) = negate (fromInteger (readDigits r))
readInt ('+':r) = fromInteger (readDigits r)
readInt r       = fromInteger (readDigits r)

-- | Convert to a 'RealFloat' exactly as @fromRational . toRational@.
toRealFloat :: RealFloat a => Scientific -> a
toRealFloat = fromRational . toRational

-- | @Just@ the value as a bounded integral type, iff it is integral AND within
-- that type's @[minBound, maxBound]@ range; @Nothing@ otherwise.
toBoundedInteger :: forall i. (Integral i, Bounded i) => Scientific -> Maybe i
toBoundedInteger s
  | isInteger s =
      let i  = integerValue s
          lo = toInteger (minBound :: i)
          hi = toInteger (maxBound :: i)
      in if i >= lo && i <= hi then Just (fromInteger i) else Nothing
  | otherwise = Nothing

-- | Floor to an 'Integer' — like 'truncateScientific' but rounds DOWN (toward
-- negative infinity) rather than toward zero, matching upstream lens-aeson's
-- @_Int@\/@_Integer@ prisms (@\"-3.7\"@ floors to @-4@; truncation would give
-- @-3@). Pure Integer 'div' (which floors), so it stays as JIT-safe as
-- 'truncateScientific'.
floorScientific :: Scientific -> Integer
floorScientific (Scientific c e)
  | e >= 0    = c * pow10 e
  | otherwise = c `div` pow10 (negate e)

-- | 'floorScientific', bounds-checked against a 'Bounded' 'Integral' type —
-- @Nothing@ if the floored value doesn't fit @[minBound, maxBound]@. Unlike
-- 'toBoundedInteger', this floors fractional inputs instead of requiring an
-- exact integer (matching the @_Int@ prism's lens-aeson semantics).
floorBoundedInteger :: forall i. (Integral i, Bounded i) => Scientific -> Maybe i
floorBoundedInteger s =
  let i  = floorScientific s
      lo = toInteger (minBound :: i)
      hi = toInteger (maxBound :: i)
  in if i >= lo && i <= hi then Just (fromInteger i) else Nothing

-- | @Right@ an integral value when the number is integral, else @Left@ the
-- floating value. (No 'Bounded' constraint here, mirroring upstream: an integral
-- value is always returned on the @Right@; a bounded @i@ can overflow.)
floatingOrInteger :: (RealFloat r, Integral i) => Scientific -> Either r i
floatingOrInteger s
  | isInteger s = Right (fromInteger (integerValue s))
  | otherwise   = Left (toRealFloat s)

-- Is the value an exact integer? True when the exponent is non-negative, or the
-- coefficient is divisible by the negative power of ten.
isInteger :: Scientific -> Bool
isInteger (Scientific c e)
  | e >= 0    = True
  | otherwise = c `rem` pow10 (negate e) == 0

-- The integer value (only meaningful when 'isInteger' holds).
integerValue :: Scientific -> Integer
integerValue (Scientific c e)
  | e >= 0    = c * pow10 e
  | otherwise = c `quot` pow10 (negate e)

-- @10 ^ n@ (n >= 0) by simple linear recursion. Deliberately NOT base's @(^)@:
-- its Integer unfolding is a mutually-recursive squaring loop with inlined
-- bignum @*@/@even@/@quot@ at ~6 sites, which explodes JIT codegen (the whole
-- Scientific→number extraction path timed out compiling a ~1300-lambda blob).
-- This single-@*@-per-step form stays small.
pow10 :: Int -> Integer
pow10 = go 1
  where
    go acc n
      | n <= 0    = acc
      | otherwise = go (acc * 10) (n - 1)

-- | Truncate toward zero to an 'Integer' via pure Integer arithmetic — NO
-- 'Double'. This is the JIT-safe integer projection: 'Double'-routed extraction
-- (@'toRealFloat' = fromRational . toRational@) drags GHC's rational→double
-- machinery into codegen and explodes it (the same GMP-adjacent path that made
-- Scientific→Double untenable on the JIT). Integer-typed decoders and the
-- @_Int@/@_Integer@ prisms use this instead of @floatingOrInteger@'s 'Double'
-- fallback, so an integer decodes without ever compiling that fallback.
truncateScientific :: Scientific -> Integer
truncateScientific = integerValue
