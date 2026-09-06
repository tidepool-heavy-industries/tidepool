{-# LANGUAGE ScopedTypeVariables #-}

module NumericContract
  ( Classification(..), classify, Renderings(..), renderings
  , doubleCases, floatCases, observations
  , doubleResult, floatResult, arithmeticResult
  ) where

import Prelude

data Classification = Classification
  { nan :: Bool, infinite :: Bool, negativeZero :: Bool }
  deriving (Eq, Show)

classify :: RealFloat a => a -> Classification
classify x = Classification (isNaN x) (isInfinite x) (isNegativeZero x)

data Renderings = Renderings String String String
  deriving (Eq, Show)

-- Ordinary Prelude Show, including a derived record; not Tidepool.Render.
renderings :: (RealFloat a, Show a) => a -> Renderings
renderings x = Renderings (show (x, classify x)) (show [x]) (show (classify x))

doubleCases :: [(String, Double)]
doubleCases = cases

floatCases :: [(String, Float)]
floatCases = cases

-- encodeFloat keeps the edge construction independent of decimal parsing.
-- Payload preservation for NaNs is not part of this contract.
cases :: forall a. RealFloat a => [(String, a)]
cases = result
  where
    result =
      [ ("positive", 1), ("negative", -1), ("positive-zero", 0)
      , ("negative-zero", negate 0), ("subnormal", encodeFloat 1 (lo - digits))
      , ("negative-subnormal", negate (encodeFloat 1 (lo - digits)))
      , ("max-finite", encodeFloat (radix ^ digits - 1) (hi - digits))
      , ("positive-infinity", 1 / 0), ("negative-infinity", -1 / 0)
      , ("nan", 0 / 0)
      ]
    witness = 1 :: a
    (lo, hi) = floatRange witness
    digits = floatDigits witness
    radix = floatRadix witness

observations :: (RealFloat a, Show a) => [(String, a)] -> [(String, Classification, Renderings)]
observations = map (\(name, x) -> (name, classify x, renderings x))

-- Derived Show must traverse numeric fields rather than only boolean records.
data NumericRecord a = NumericRecord { numericField :: a } deriving Show

numericSummary :: (RealFloat a, Show a) => [(String, a)] -> String
numericSummary xs = show (observations xs, map (NumericRecord . snd) xs)

doubleResult :: String
doubleResult = numericSummary doubleCases

floatResult :: String
floatResult = numericSummary floatCases

-- Only bounded, finite operands are rounded or converted to integral values.
-- Arithmetic is observed independently of floating Show/classification.
{-# NOINLINE arithmetic #-}
arithmetic :: (RealFloat a) => a -> [Bool]
arithmetic witness =
  [ x + y == 5, x - y == -1, x * y == 6, x / y == 2 / 3
  , x < y, not (x > y), x == x, x /= y
  , truncate (witness + 1.75) == (2 :: Integer)
  , floor (witness - 3.75) == (-3 :: Integer)
  , ceiling (witness - 3.75) == (-2 :: Integer)
  , round (witness + 1.5) == (2 :: Integer)
  , round (witness + 2.5) == (4 :: Integer)
  , let (m,e) = decodeFloat x in encodeFloat m e == x
  ]
  where
    x = witness + 1
    y = witness + 2

-- Keep conversion inputs opaque to simplification at the call site. Unlike
-- asTypeOf, each helper consumes the actual operand.
{-# NOINLINE intToDouble #-}
intToDouble :: Int -> Double
intToDouble = fromIntegral

{-# NOINLINE intToFloat #-}
intToFloat :: Int -> Float
intToFloat = fromIntegral

{-# NOINLINE widen #-}
widen :: Float -> Double
widen = realToFrac

{-# NOINLINE narrow #-}
narrow :: Double -> Float
narrow = realToFrac

arithmeticResult :: String
arithmeticResult = show
  ( arithmetic (1 :: Double), arithmetic (1 :: Float)
  , widen 1.25 == 1.25
  , narrow 1.25 == 1.25
  , intToDouble 14592 == 14592, intToFloat 14592 == 14592
  )
