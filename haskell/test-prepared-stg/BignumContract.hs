module BignumContract where

import Data.Text (Text)
import Data.Text qualified as T

-- Integer arithmetic that must leave Int64. Results are observed as Text,
-- Int, tuples, or Bool because Integer itself has no observation kind.
pow2ToHundredText :: Text
pow2ToHundredText = T.pack (show (2 ^ (100 :: Int) :: Integer))

factorial30Text :: Text
factorial30Text = T.pack (show (product [1 .. 30 :: Integer]))

-- Product of large primes reduced into Int range via mod.
primeProductMod :: Int
primeProductMod =
  fromIntegral
    (product [2305843009213693951, 618970019642690137449562111, 162259276829213363391578010288127 :: Integer]
      `mod` 1000000007)

-- gcd on values that exceed 64 bits; the answer itself exceeds 64 bits.
gcdLargeText :: Text
gcdLargeText =
  T.pack (show (gcd (3 * 2 ^ (80 :: Int)) (21 * 2 ^ (70 :: Int) :: Integer)))

-- Negative large division and mod, reduced to an observable pair.
negativeQuotRem :: (Int, Int)
negativeQuotRem =
  ( fromIntegral (quotient `mod` 1000000007)
  , fromIntegral remainder
  )
  where
    (quotient, remainder) =
      (negate (2 ^ (90 :: Int)) :: Integer) `divMod` 1000003

-- Comparison between a large Integer and a small one.
largeVersusSmall :: Bool
largeVersusSmall = (2 ^ (100 :: Int) :: Integer) > 42
