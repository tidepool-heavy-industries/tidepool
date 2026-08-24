{-# LANGUAGE ScopedTypeVariables #-}

-- | Randomness: the canonical @System.Random@ vocabulary
-- ('StdGen'\/'mkStdGen'\/'randomR'\/'randoms'\/'split'\/'newStdGen'\/'randomRIO'),
-- backed by the real @random@\/@splitmix@ packages — 'StdGen', 'mkStdGen',
-- and 'split' are straight re-exports, so 'StdGen' really is @splitmix@'s
-- generator. 'randomR'\/'random' stay a thin local 'Random' class over the
-- package's own 'System.Random.genWord64' primitive rather than re-exporting
-- the package's 'Random' class wholesale: that class's generic, range-width-
-- adaptive default methods reach @Word16#@ machinery the JIT does not
-- implement, where 'genWord64' (an unconditional full-word draw) does not.
-- 'newStdGen'\/'randomRIO' are the one effectful seam: rather than the
-- packages' own global mutable generator, they seed a fresh 'StdGen' from
-- 'Entropy'\'s single verb (fresh OS-entropy seed material).
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
import Data.Bits (shiftR)
import System.Random (StdGen, mkStdGen, split, genWord64)
import Control.Monad.Freer (Eff, Member)
import Tidepool.Effects (Entropy, entropySeed)

-- | A type drawable from a 'StdGen'. 'randomR' draws within an inclusive
-- range (bounds may be given in either order); 'random' draws from the
-- type's full range ('Double' uniform over @[0, 1)@).
class Random a where
  randomR :: (a, a) -> StdGen -> (a, StdGen)
  random :: StdGen -> (a, StdGen)

instance Random Int where
  randomR (lo, hi) g =
    let (lo', hi') = if lo <= hi then (lo, hi) else (hi, lo)
        (w, g') = genWord64 g
        range = fromIntegral hi' - fromIntegral lo' + 1 :: Word
        v = lo' + fromIntegral (fromIntegral w `mod` range :: Word)
    in (v, g')
  random g = let (w, g') = genWord64 g in (fromIntegral w, g')

instance Random Double where
  randomR (lo, hi) g =
    let (lo', hi') = if lo <= hi then (lo, hi) else (hi, lo)
        (w, g') = genWord64 g
        -- Top 53 bits / 2^53: a uniform Double in [0, 1).
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
