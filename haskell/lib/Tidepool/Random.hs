{-# LANGUAGE ScopedTypeVariables #-}

-- | Randomness: the canonical @System.Random@ vocabulary
-- ('StdGen'\/'mkStdGen'\/'randomR'\/'randoms'\/'split'\/'newStdGen'\/'randomRIO'),
-- backed by a pure generator written directly against this module rather
-- than the @random@\/@splitmix@ packages themselves — those pull in
-- @Word64#@ primops (@plusWord64#@, @timesWord64#@) the JIT does not
-- implement. Plain @Word@ (machine-word, 64-bit) arithmetic compiles and
-- runs directly, so the generator below is the SplitMix64 algorithm
-- (Steele\/Vigna — the same one @random >= 1.2@ itself uses) written over
-- 'Data.Bits' on 'Word'.
--
-- 'newStdGen'\/'randomRIO' are the one effectful seam, built on 'Entropy'\'s
-- single verb (fresh OS-entropy seed material) — everything else here is
-- ordinary pure Haskell.
module Tidepool.Random
  ( StdGen
  , mkStdGen
  , Random(..)
  , randoms
  , split
  , newStdGen
  , randomRIO
  ) where

import Prelude
import Data.Bits (shiftR, xor)
import Control.Monad.Freer (Eff, Member)
import Tidepool.Effects (Entropy, entropySeed)

-- | Opaque pure generator state.
newtype StdGen = StdGen Word

-- | Seed a generator deterministically: the same seed always replays the
-- same 'randomR'\/'random'\/'randoms'\/'split' sequence.
mkStdGen :: Int -> StdGen
mkStdGen seed = StdGen (fromIntegral seed)

-- Golden-ratio increment (SplitMix64) — odd, so the sequence of states
-- visits the full 64-bit period before repeating.
goldenGamma :: Word
goldenGamma = 0x9E3779B97F4A7C15

-- SplitMix64's finalizing bit-mixer (Murmur3-style avalanche): turns a
-- linearly-incrementing counter into output with no short-range structure.
mix64 :: Word -> Word
mix64 z0 =
  let z1 = (z0 `xor` (z0 `shiftR` 30)) * 0xBF58476D1CE4E5B9
      z2 = (z1 `xor` (z1 `shiftR` 27)) * 0x94D049BB133111EB
  in z2 `xor` (z2 `shiftR` 31)

nextWord :: StdGen -> (Word, StdGen)
nextWord (StdGen s) =
  let s' = s + goldenGamma
  in (mix64 s', StdGen s')

-- | Two generators derived from one, independent of each other: advancing
-- one does not affect the other's sequence.
split :: StdGen -> (StdGen, StdGen)
split g =
  let (w, g') = nextWord g
      s2 = mix64 (w `xor` 0xBF58476D1CE4E5B9)
  in (g', StdGen s2)

-- | A type drawable from a 'StdGen'. 'randomR' draws within an inclusive
-- range (bounds may be given in either order); 'random' draws from the
-- type's full range ('Double' uniform over @[0, 1)@).
class Random a where
  randomR :: (a, a) -> StdGen -> (a, StdGen)
  random :: StdGen -> (a, StdGen)

instance Random Int where
  randomR (lo, hi) g =
    let (lo', hi') = if lo <= hi then (lo, hi) else (hi, lo)
        (w, g') = nextWord g
        range = fromIntegral hi' - fromIntegral lo' + 1 :: Word
        v = lo' + fromIntegral (w `mod` range)
    in (v, g')
  random g = let (w, g') = nextWord g in (fromIntegral w, g')

instance Random Double where
  randomR (lo, hi) g =
    let (lo', hi') = if lo <= hi then (lo, hi) else (hi, lo)
        (w, g') = nextWord g
        -- Top 53 bits / 2^53: a uniform Double in [0, 1), the same technique
        -- base's own `random` package uses.
        frac = fromIntegral (w `shiftR` 11) / 9007199254740992 :: Double
    in (lo' + frac * (hi' - lo'), g')
  random = randomR (0.0, 1.0)

-- | Infinite list of full-range draws (see 'random').
randoms :: forall a. Random a => StdGen -> [a]
randoms g = let (x, g') = random g in x : randoms g'

-- | A generator seeded from fresh OS entropy.
newStdGen :: forall effs. Member Entropy effs => Eff effs StdGen
newStdGen = mkStdGen <$> entropySeed

-- | One value drawn from a fresh, OS-entropy-seeded generator.
randomRIO :: forall a effs. (Random a, Member Entropy effs) => (a, a) -> Eff effs a
randomRIO bounds = do
  g <- newStdGen
  pure (fst (randomR bounds g))
