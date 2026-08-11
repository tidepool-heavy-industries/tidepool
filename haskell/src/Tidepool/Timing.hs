-- | Env-gated, wire-format-inert phase timing for @tidepool-extract@
-- (@TIDEPOOL_TIMING=1@). MEASUREMENT ONLY — this module never changes what
-- gets computed, only whether a stderr line is written about how long it
-- took. See @tidepool-harness\/src\/timing.rs@ for the authoritative
-- @PHASE_*@ name vocabulary and the stderr grammar this emits
-- (@tidepool-timing phase=\<name\> ms=\<int\>@); the two must stay in sync by
-- hand since they live in separate languages/crates.
--
-- __Phases are FLAT, never nested (do not sum a phase into another).__ On
-- both @GhcPipeline.hs@ extraction variants (@normalVariant@ and
-- @sessionVariant@, which share the one @runCompile@ skeleton),
-- @ghc_setup@ (session DynFlags setup +
-- guessTarget\/setTargets + @depanal@) and @ghc_load@ (the @load'@ call
-- alone) are two SEPARATE, NON-OVERLAPPING spans that PARTITION what an
-- older @ghc_session@ bracket used to cover on the compile lane — see the
-- @PHASE_GHC_SESSION@ tombstone in @tidepool-harness\/src\/timing.rs@. A
-- collector recovers the old coarse figure as the SUM @ghc_setup + ghc_load@
-- (a flat-sum collector already does this for free); neither row is emitted
-- twice, so there is nothing to avoid double-counting. On the session path,
-- @inject@ (PHASE 2's Val-iface splice) is a third flat row alongside them,
-- with no normal-path counterpart (it is emitted by @sessionVariant@'s
-- @cpAfterLoad@ hook). @load'@ itself gets NO internal
-- decomposition: it already redoes the SAME parse\/typecheck\/core2core work
-- the per-module loop below it redoes a second time, so one row around the
-- whole call answers what matters. Do not redefine an existing phase's
-- MEANING when adding a finer one — see the @classify_extract@ RETIRED
-- tombstone in @plans\/self-iterating-harness\/11-extract-timing-contract.md@
-- for why a same-named stage carrying a different meaning silently poisons
-- longitudinal comparison; new granularity gets a NEW name instead (as
-- @ghc_setup@\/@ghc_load@ did here, rather than repurposing @ghc_session@).
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
