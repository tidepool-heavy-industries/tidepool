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
  , emitModuleInterfaceTiming
  , InterfaceStage(..)
  , InterfaceReuse(..)
  , measureModuleInterface
  , newTimingRequestIdentity
  , monotonicTime
  , elapsedMs
    -- * Opt-in memo trace (TIDEPOOL_MEMO_TRACE=1)
  , readMemoTraceEnabled
  , emitMemoCycleGraph
  , emitMemoMissTrace
  ) where

import Control.Monad.IO.Class (MonadIO, liftIO)
import Data.List (intercalate)
import Data.Word (Word64)
import GHC.Clock (getMonotonicTime, getMonotonicTimeNSec)
import GHC.Fingerprint.Type (Fingerprint)
import qualified GHC.Stats as RTS
import System.Environment (lookupEnv)
import System.IO (hPutStrLn, stderr)
import System.CPUTime (getCPUTime)

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
-- the operator-visible-by-default line: an operator reading plain harness
-- logs sees module count, wall time, the
-- typecheck/lowering phase split, and the top-3 modules by wall time with NO
-- env var to set. Deliberately ONE line, not a dump — the per-module BREAKDOWN
-- (every module, not just the top 3) stays behind 'readTimingEnabled' via
-- 'emitModuleTiming', same discipline as every other detailed diagnostic here.
-- Distinct wire prefix (@tidepool-compile-summary@, not @tidepool-timing @)
-- so 'ExtractTiming::parse' on the Rust side (which matches the
-- @tidepool-timing \<space\>@ prefix only) never sees or misparses this line.
emitCompileSummary :: Int -> Integer -> Integer -> Integer -> Integer
  -> [(String, Integer)] -> [(String, Integer)] -> IO ()
emitCompileSummary moduleCount wallMs typecheckMs loweringMs interfaceMs topModules topInterfaces =
  hPutStrLn stderr $
    "tidepool-compile-summary modules=" ++ show moduleCount
    ++ " wall_ms=" ++ show wallMs
    ++ " typecheck_ms=" ++ show typecheckMs
    ++ " lowering_ms=" ++ show loweringMs
    ++ " interface_ms=" ++ show interfaceMs
    ++ " top=" ++ intercalate "," [ name ++ ":" ++ show ms | (name, ms) <- topModules ]
    ++ " interface_top=" ++ intercalate "," [ name ++ ":" ++ show ms | (name, ms) <- topInterfaces ]

-- | Write one @tidepool-timing-module module=\<name\> ms=\<ms\>@ line per
-- module, only when @enabled@ — the full per-module breakdown backing
-- 'emitCompileSummary''s top-3, kept behind 'readTimingEnabled' like every
-- other detailed diagnostic in this module (the compile-attribution
-- measurement run is the intended reader). Distinct prefix from both
-- @tidepool-timing \<space\>@ (the phase wire grammar) and
-- @tidepool-compile-summary@ — a collector keyed on either never picks this
-- line up by accident.
emitModuleTiming :: Bool -> [(String, Integer)] -> [(String, Integer)] -> IO ()
emitModuleTiming False _ _ = pure ()
emitModuleTiming True modules interfaces =
  mapM_ (\(name, ms) ->
    hPutStrLn stderr ("tidepool-timing-module module=" ++ name ++ " ms=" ++ show ms
      ++ " interface_ms=" ++ show (lookupInterface name))) modules
  where
    lookupInterface name = maybe 0 id (lookup name interfaces)

-- | Detail rows identify the module which paid the interface work. They are
-- diagnostic-only and deliberately do not create a flat timing phase.
emitModuleInterfaceTiming :: Bool -> String -> String -> String -> Integer -> IO ()
emitModuleInterfaceTiming False _ _ _ _ = pure ()
emitModuleInterfaceTiming True moduleName parent phase ms =
  hPutStrLn stderr ("tidepool-timing-module-detail module=" ++ moduleName
    ++ " parent=" ++ parent ++ " phase=" ++ phase ++ " ms=" ++ show ms)

-- | The two compiler stages that construct an in-memory interface themselves.
-- Keeping this closed prevents diagnostic spelling from becoming control flow.
data InterfaceStage
  = CheckedEnvironmentInterface
  | SessionRegistrationInterface

data InterfaceReuse
  = HptMiss
  | MemoMiss
  | MemoDisabled

-- | A process-local identity for one compiler cycle. Compiler requests are
-- serialized, so their monotonic start stamps distinguish direct requests and
-- resident transactions without adding diagnostic identity to the protocol.
newTimingRequestIdentity :: IO Word64
newTimingRequestIdentity = getMonotonicTimeNSec

-- | Measure one @mkIfaceTc@ call. Wall time comes from the monotonic clock and
-- CPU time is process CPU. Allocation and GC counters are process-wide deltas
-- from GHC's RTS statistics. They are available only when the worker started
-- with RTS statistics enabled. This samples existing counters and never forces
-- a collection.
measureModuleInterface
  :: Bool -> Word64 -> String -> InterfaceStage -> InterfaceReuse
  -> IO a -> IO (a, Integer)
measureModuleInterface enabled request moduleName stage reuse action = do
  wall0 <- getMonotonicTimeNSec
  cpu0 <- if enabled then Just <$> getCPUTime else pure Nothing
  rts0 <- if enabled then readRtsStats else pure Nothing
  result <- action
  wall1 <- getMonotonicTimeNSec
  cpu1 <- if enabled then Just <$> getCPUTime else pure Nothing
  rts1 <- if enabled then readRtsStats else pure Nothing
  let wallNs = delta wall0 wall1
      wallMs = fromIntegral ((wallNs + 500000) `div` 1000000)
  if enabled
    then hPutStrLn stderr (renderInterfaceMeasurement request moduleName stage reuse
      wallNs (cpuDelta cpu0 cpu1) rts0 rts1)
    else pure ()
  pure (result, wallMs)

readRtsStats :: IO (Maybe RTS.RTSStats)
readRtsStats = do
  available <- RTS.getRTSStatsEnabled
  if available then Just <$> RTS.getRTSStats else pure Nothing

renderInterfaceMeasurement
  :: Word64 -> String -> InterfaceStage -> InterfaceReuse -> Word64 -> Integer
  -> Maybe RTS.RTSStats -> Maybe RTS.RTSStats -> String
renderInterfaceMeasurement request moduleName stage reuse wallNs cpuNs before after =
  "tidepool-timing-module-detail module=" ++ moduleName
    ++ " request=" ++ show request
    ++ " parent=module_interface phase=make_iface"
    ++ " stage=" ++ renderStage stage
    ++ " reuse=" ++ renderReuse reuse
    ++ " ms=" ++ show ((wallNs + 500000) `div` 1000000)
    ++ " wall_ns=" ++ show wallNs
    ++ " cpu_ns=" ++ show cpuNs
    ++ case (before, after) of
      (Just rts0, Just rts1) ->
        " rts=enabled rts_scope=process_delta"
          ++ " allocated_bytes=" ++ show (delta (RTS.allocated_bytes rts0) (RTS.allocated_bytes rts1))
          ++ " gc_cpu_ns=" ++ show (delta (RTS.gc_cpu_ns rts0) (RTS.gc_cpu_ns rts1))
          ++ " gc_elapsed_ns=" ++ show (delta (RTS.gc_elapsed_ns rts0) (RTS.gc_elapsed_ns rts1))
          ++ " gcs=" ++ show (delta (RTS.gcs rts0) (RTS.gcs rts1))
      _ ->
        " rts=unavailable rts_scope=process_delta"
          ++ " allocated_bytes=unavailable gc_cpu_ns=unavailable"
          ++ " gc_elapsed_ns=unavailable gcs=unavailable"

renderStage :: InterfaceStage -> String
renderStage CheckedEnvironmentInterface = "checked_environment"
renderStage SessionRegistrationInterface = "session_registration"

renderReuse :: InterfaceReuse -> String
renderReuse HptMiss = "hpt_miss"
renderReuse MemoMiss = "memo_miss"
renderReuse MemoDisabled = "memo_disabled"

cpuDelta :: Maybe Integer -> Maybe Integer -> Integer
cpuDelta (Just before) (Just after) = delta before after `div` 1000
cpuDelta _ _ = 0

delta :: (Num a, Ord a) => a -> a -> a
delta before after
  | after >= before = after - before
  | otherwise = 0

-- | Read the @TIDEPOOL_MEMO_TRACE@ env var. On iff exactly @"1"@; unset or
-- any other value is off. Independent of 'readTimingEnabled': a diagnostic
-- run can enable one, both, or neither.
readMemoTraceEnabled :: IO Bool
readMemoTraceEnabled = (== Just "1") <$> lookupEnv "TIDEPOOL_MEMO_TRACE"

-- | One line per module in the selected module graph, written once per
-- compile cycle (never per lookup) when @TIDEPOOL_MEMO_TRACE=1@. Graph
-- capture holds paths and fingerprints only, never source or Core.
emitMemoCycleGraph
  :: Bool -> Word64 -> String -> String -> Maybe FilePath -> Maybe FilePath
  -> Fingerprint -> [String] -> String -> IO ()
emitMemoCycleGraph False _ _ _ _ _ _ _ _ = pure ()
emitMemoCycleGraph True cycleId moduleName sourceKind selectedPath resolvedPath
  sourceFingerprint directDeps dependencyDigest =
  hPutStrLn stderr $
    "tidepool-memo-cycle-graph cycle=" ++ show cycleId
      ++ " module=" ++ moduleName
      ++ " source_kind=" ++ sourceKind
      ++ " selected_path=" ++ maybe "<none>" id selectedPath
      ++ " resolved_path=" ++ maybe "<none>" id resolvedPath
      ++ " fingerprint=" ++ show sourceFingerprint
      ++ " direct_deps=" ++ intercalate "," directDeps
      ++ " dependency_digest=" ++ dependencyDigest

-- | One line per memo miss, written when @TIDEPOOL_MEMO_TRACE=1@. Distinct
-- from the always-under-'TIDEPOOL_TIMING' @tidepool-memo-miss@ summary line:
-- this one names the originating cycle that produced the stale entry, and
-- for a dependency-witness change lists exactly which witnesses were added,
-- removed, or changed — with path and fingerprint reported separately so a
-- path-only change (identical fingerprint, different path) is distinguishable
-- from a real content change.
emitMemoMissTrace
  :: Bool -> Word64 -> String -> String -> String -> [String] -> [String] -> [String] -> IO ()
emitMemoMissTrace False _ _ _ _ _ _ _ = pure ()
emitMemoMissTrace True cycleId originatingCycle moduleName reason added removed changed =
  hPutStrLn stderr $
    "tidepool-memo-trace-miss cycle=" ++ show cycleId
      ++ " originating_cycle=" ++ originatingCycle
      ++ " module=" ++ moduleName
      ++ " reason=" ++ reason
      ++ " added=" ++ intercalate ";" added
      ++ " removed=" ++ intercalate ";" removed
      ++ " changed=" ++ intercalate ";" changed
