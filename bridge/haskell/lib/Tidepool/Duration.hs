-- | Dimensional durations for model-facing APIs.
--
-- Constructors are hidden so a bare integer cannot accidentally cross a
-- deadline boundary. Zero is an explicit immediate deadline; the public
-- constructors accept 'Natural', so negative durations are not representable.
module Tidepool.Duration
  ( Duration
  , milliseconds
  , seconds
  , minutes
  , toMilliseconds
  ) where

import Numeric.Natural (Natural)
import Prelude

data Duration
  = DurationMilliseconds Int
  | DurationSeconds Int
  | DurationMinutes Int
  deriving (Show)

instance Eq Duration where
  left == right = magnitude left == magnitude right

instance Ord Duration where
  compare left right = compare (magnitude left) (magnitude right)

-- Normalize after widening: even valid unit counts may exceed Int when scaled.
magnitude :: Duration -> Integer
magnitude (DurationMilliseconds value) = toInteger value
magnitude (DurationSeconds value) = toInteger value * 1000
magnitude (DurationMinutes value) = toInteger value * 60000

-- | Convert to the runtime's millisecond representation without overflowing
-- while scaling a valid seconds or minutes count.
toMilliseconds :: Duration -> Maybe Int
toMilliseconds duration
  | value > toInteger (maxBound :: Int) = Nothing
  | otherwise = Just (fromInteger value)
  where
    value = magnitude duration

milliseconds :: Natural -> Duration
milliseconds = DurationMilliseconds . bounded

seconds :: Natural -> Duration
seconds = DurationSeconds . bounded

minutes :: Natural -> Duration
minutes = DurationMinutes . bounded

bounded :: Natural -> Int
bounded value
  | value > fromIntegral (maxBound :: Int) = error "duration exceeds the runtime integer range"
  | otherwise = fromIntegral value
