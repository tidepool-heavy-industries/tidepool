-- | Opt-in compiler measurements. Flat phases use @tidepool-timing@;
-- nested breakdowns use @tidepool-timing-detail parent=...@ and must not
-- be added to their parent. Counts use @tidepool-count@, never milliseconds.
-- The frontend retains these lines within each request's diagnostic output.
module Tidepool.Timing
  ( readTimingEnabled
  , timePhase
  , timeDetailPhase
  , timeSection
  , emitPhase
  , emitDetailPhase
  , emitCount
  , emitCompileSummary
  , emitModuleTiming
  , monotonicTime
  , elapsedMs
  ) where

import Control.Monad.IO.Class (MonadIO, liftIO)
import Data.List (intercalate)
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
timePhase :: MonadIO m => Bool -> String -> m a -> m a
timePhase enabled name act = do
  (r, ms) <- timeSection act
  liftIO (emitPhase enabled name ms)
  pure r

-- | Measure a named child without putting it in the flat phase stream.
timeDetailPhase :: MonadIO m => Bool -> String -> String -> m a -> m a
timeDetailPhase enabled parent name act = do
  (result, ms) <- timeSection act
  liftIO (emitDetailPhase enabled parent name ms)
  pure result

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

-- | Explicitly nested timings stay out of flat-sum collectors.
emitDetailPhase :: Bool -> String -> String -> Integer -> IO ()
emitDetailPhase False _ _ _ = pure ()
emitDetailPhase True parent name ms =
  hPutStrLn stderr ("tidepool-timing-detail parent=" ++ parent
    ++ " phase=" ++ name ++ " ms=" ++ show ms)

emitCount :: Bool -> String -> Integer -> IO ()
emitCount False _ _ = pure ()
emitCount True name count =
  hPutStrLn stderr ("tidepool-count name=" ++ name ++ " count=" ++ show count)

elapsedMs :: Double -> Double -> Integer
elapsedMs t0 t1 = round ((t1 - t0) * 1000)

-- | Write the ONE compact per-compile summary line — ALWAYS, unlike every
-- other emitter in this module: not gated by 'readTimingEnabled'. This is
-- the operator-visible-by-default line (compile-attribution lane,
-- plans/turn-latency-state-injection.md's companion measurement): an
-- operator reading plain harness logs sees module count, wall time, the
-- typecheck/lowering phase split, and the top-3 modules by wall time with NO
-- env var to set. Deliberately ONE line, not a dump — the per-module BREAKDOWN
-- (every module, not just the top 3) stays behind 'readTimingEnabled' via
-- 'emitModuleTiming', same discipline as every other detailed diagnostic here.
-- Distinct wire prefix (@tidepool-compile-summary@, not @tidepool-timing @)
-- so 'ExtractTiming::parse' on the Rust side (which matches the
-- @tidepool-timing \<space\>@ prefix only) never sees or misparses this line.
emitCompileSummary :: Int -> Integer -> Integer -> Integer -> [(String, Integer)] -> IO ()
emitCompileSummary moduleCount wallMs typecheckMs loweringMs topModules =
  hPutStrLn stderr $
    "tidepool-compile-summary modules=" ++ show moduleCount
    ++ " wall_ms=" ++ show wallMs
    ++ " typecheck_ms=" ++ show typecheckMs
    ++ " lowering_ms=" ++ show loweringMs
    ++ " top=" ++ intercalate "," [ name ++ ":" ++ show ms | (name, ms) <- topModules ]

-- | Write one @tidepool-timing-module module=\<name\> ms=\<ms\>@ line per
-- module, only when @enabled@ — the full per-module breakdown backing
-- 'emitCompileSummary''s top-3, kept behind 'readTimingEnabled' like every
-- other detailed diagnostic in this module (the compile-attribution
-- measurement run is the intended reader). Distinct prefix from both
-- @tidepool-timing \<space\>@ (the phase wire grammar) and
-- @tidepool-compile-summary@ — a collector keyed on either never picks this
-- line up by accident.
emitModuleTiming :: Bool -> [(String, Integer)] -> IO ()
emitModuleTiming False _ = pure ()
emitModuleTiming True modules =
  mapM_ (\(name, ms) ->
    hPutStrLn stderr ("tidepool-timing-module module=" ++ name ++ " ms=" ++ show ms))
    modules
