{-# LANGUAGE ScopedTypeVariables #-}

module NumericContract
  ( Classification(..), classify, Renderings(..), renderings
  , doubleCases, floatCases, observations
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
