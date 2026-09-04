-- | Dimensional durations for model-facing APIs.
--
-- Constructors are hidden so a bare integer cannot accidentally cross a
-- deadline boundary. Zero is an explicit immediate deadline; the public
-- constructors accept 'Natural', so negative durations are not representable.
module Tidepool.Duration
  ( Duration
  , RequestDeadline
  , milliseconds
  , seconds
  , minutes
  , after
  ) where

import Numeric.Natural (Natural)
import Prelude

data Duration
  = DurationMilliseconds Int
  | DurationSeconds Int
  | DurationMinutes Int
  deriving (Show, Eq, Ord)

newtype RequestDeadline = RequestDeadline Duration
  deriving (Show, Eq, Ord)

milliseconds :: Natural -> Duration
milliseconds = DurationMilliseconds . bounded

seconds :: Natural -> Duration
seconds = DurationSeconds . bounded

minutes :: Natural -> Duration
minutes = DurationMinutes . bounded

after :: Duration -> RequestDeadline
after = RequestDeadline

bounded :: Natural -> Int
bounded value
  | value > fromIntegral (maxBound :: Int) = error "duration exceeds the runtime integer range"
  | otherwise = fromIntegral value
