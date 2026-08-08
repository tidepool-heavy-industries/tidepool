-- | Env-gated, wire-format-inert phase timing for @tidepool-extract@
-- (@TIDEPOOL_TIMING=1@). MEASUREMENT ONLY — this module never changes what
-- gets computed, only whether a stderr line is written about how long it
-- took. See @tidepool-harness\/src\/timing.rs@ for the authoritative
-- @PHASE_*@ name vocabulary and the stderr grammar this emits
-- (@tidepool-timing phase=\<name\> ms=\<int\>@); the two must stay in sync by
-- hand since they live in separate languages/crates.
module Tidepool.Timing
  ( readTimingEnabled
  , timePhase
  , timeSection
  , emitPhase
  , monotonicTime
  , elapsedMs
  ) where

import Control.Monad.IO.Class (MonadIO, liftIO)
import GHC.Clock (getMonotonicTime)
import System.Environment (lookupEnv)
import System.IO (hPutStrLn, stderr)

-- | Read the @TIDEPOOL_TIMING@ env var. On iff exactly @"1"@; unset or any
-- other value is off.
readTimingEnabled :: IO Bool
readTimingEnabled = (== Just "1") <$> lookupEnv "TIDEPOOL_TIMING"

-- | Run @act@ and, when @enabled@, write one
-- @tidepool-timing phase=\<name\> ms=\<int\>@ line to stderr AFTER @act@
-- completes. Emits nothing when @enabled@ is 'False' — the only difference
-- between the two is that one stderr line.
timePhase :: Bool -> String -> IO a -> IO a
timePhase enabled name act = do
  (r, ms) <- timeSection act
  emitPhase enabled name ms
  pure r

-- | Time @act@ without emitting anything — for a phase whose wall clock is
-- the SUM of several non-contiguous sub-steps (e.g. once per module in a
-- compile loop). The caller accumulates the returned milliseconds across
-- calls and emits once via 'emitPhase' after the loop. Generic over
-- 'MonadIO' so it works both in plain @IO@ (Main.hs) and inside the @Ghc@
-- session monad (GhcPipeline.hs, Binders.hs) without a separate lift.
timeSection :: MonadIO m => m a -> m (a, Integer)
timeSection act = do
  t0 <- liftIO getMonotonicTime
  r <- act
  t1 <- liftIO getMonotonicTime
  pure (r, elapsedMs t0 t1)

-- | The current monotonic clock reading, for a call site that needs to
-- bracket a span spread across several statements it can't cleanly nest
-- into one 'timeSection' block (e.g. a boundary between two bound
-- variables both needed afterward). Prefer 'timeSection'/'timePhase' where
-- the span nests cleanly.
monotonicTime :: MonadIO m => m Double
monotonicTime = liftIO getMonotonicTime

-- | Write one @tidepool-timing phase=<name> ms=<ms>@ line, only when
-- @enabled@ — the single call site that owns the wire grammar, so every
-- phase line in the binary is byte-for-byte the same shape.
emitPhase :: Bool -> String -> Integer -> IO ()
emitPhase False _ _ = pure ()
emitPhase True name ms =
  hPutStrLn stderr ("tidepool-timing phase=" ++ name ++ " ms=" ++ show ms)

elapsedMs :: Double -> Double -> Integer
elapsedMs t0 t1 = round ((t1 - t0) * 1000)
