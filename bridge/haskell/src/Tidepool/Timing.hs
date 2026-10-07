-- | Opt-in compiler measurements. Flat phases use @tidepool-timing@;
-- nested breakdowns use @tidepool-timing-detail parent=...@ and must not
-- be added to their parent. Counts use @tidepool-count@, never milliseconds.
-- The frontend retains these lines within each request's diagnostic output.
module Tidepool.Timing
  ( readTimingEnabled
  , timePhase
  , timeDetailPhase
  , timeModuleDetailPhase
  , ResourceTimingStart
  , beginResourceTiming
  , endResourceTiming
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
  , ReuseStage(..), ReuseDecision(..), ReuseReason(..), ReuseVersionKind(..)
  , ReuseModule(..), ReuseContext(..), emitReuse, emitReuseComplete, emitCheckOnlyReuseApplicability
    -- * Opt-in memo trace (TIDEPOOL_MEMO_TRACE=1)
  , readMemoTraceEnabled
  , MemoSelectionState(..), MemoSelectionTrace(..), MemoExecutableTrace(..)
  , emitMemoCycleGraph
  , emitMemoMissTrace
  ) where

import Control.Monad.IO.Class (MonadIO, liftIO)
import Data.List (intercalate)
import Data.Word (Word64)
import GHC.Clock (getMonotonicTime, getMonotonicTimeNSec)
import GHC.Fingerprint.Type (Fingerprint)
import GHC.Unit.Module (Module, moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Types (unitString)
import qualified GHC.Stats as RTS
import System.Environment (lookupEnv)
import System.IO (hPutStrLn, stderr)
import System.CPUTime (getCPUTime)
import Tidepool.Json (jsonString)

-- These events describe one owner's decision, never a whole-compile hit.
-- Transport identity belongs to the enclosing daemon trace; cycle identifies
-- the worker operation within that request. Versions are opaque owner seals.
data ReuseStage = SourceFrontend | Interface | FinalizedCore | PreparedBody
  | SiteWitness | OriginalRecovery | RawProjection | ArtifactReference
  | ArtifactTransfer | NativeImage deriving (Eq, Show)
data ReuseDecision = ReuseHit | ReuseMiss | ReuseWork | ReuseDisabled
  | ReuseEvicted | ReuseEpochRotated | ReuseComplete | ReuseNotApplicable deriving (Eq, Show)
data ReuseReason = Matched | Absent | ChangedSource | ChangedDependency
  | ChangedAuthority | ThFresh | Epoch | Recovery | CacheDisabled | Evicted
  | StageComplete | CheckOnlyStage deriving (Eq, Show)
data ReuseVersionKind = SourceFingerprint | CanonicalSeal | PreparedIdentity
  | InterfaceFingerprint | ImageIdentity deriving (Eq, Show)
data ReuseModule = ReuseModule String String ReuseVersionKind String | ReuseImage String
data ReuseContext = ReuseContext Word64 String

reuseStageName :: ReuseStage -> String
reuseStageName stage = case stage of
  SourceFrontend -> "source_frontend"; Interface -> "interface"
  FinalizedCore -> "finalized_core"; PreparedBody -> "prepared_body"
  SiteWitness -> "site_witness"; OriginalRecovery -> "original_recovery"
  RawProjection -> "raw_projection"; ArtifactReference -> "artifact_reference"
  ArtifactTransfer -> "artifact_transfer"; NativeImage -> "native_image"

reuseDecisionName :: ReuseDecision -> String
reuseDecisionName decision = case decision of
  ReuseHit -> "hit"; ReuseMiss -> "miss"; ReuseWork -> "work"
  ReuseDisabled -> "disabled"; ReuseEvicted -> "evicted"
  ReuseEpochRotated -> "epoch_rotated"; ReuseComplete -> "complete"
  ReuseNotApplicable -> "not_applicable"

reuseReasonName :: ReuseReason -> String
reuseReasonName reason = case reason of
  Matched -> "matched"; Absent -> "absent"; ChangedSource -> "changed_source"
  ChangedDependency -> "changed_dependency"; ChangedAuthority -> "changed_authority"
  ThFresh -> "th_fresh"; Epoch -> "epoch"; Recovery -> "recovery"
  CacheDisabled -> "cache_disabled"; Evicted -> "evicted"; StageComplete -> "stage_complete"
  CheckOnlyStage -> "check_only"

reuseVersionName :: ReuseVersionKind -> String
reuseVersionName kind = case kind of
  SourceFingerprint -> "source_fingerprint"; CanonicalSeal -> "canonical_seal"
  PreparedIdentity -> "prepared_identity"; ImageIdentity -> "image_identity"
  InterfaceFingerprint -> "interface_fingerprint"

emitReuse :: Bool -> ReuseContext -> ReuseStage -> ReuseDecision -> ReuseReason
  -> Maybe ReuseModule -> Word64 -> Maybe Word64 -> IO ()
emitReuse False _ _ _ _ _ _ _ = pure ()
emitReuse True (ReuseContext cycleId purpose) stage decision reason owner items bytes = do
  observed <- getMonotonicTimeNSec
  let moduleFields = case owner of
        Nothing -> [("unit","null"),("module","null"),("version_kind","null"),("version","null")]
        Just (ReuseModule unit modul kind version) ->
          [("unit",jsonString unit),("module",jsonString modul)
          ,("version_kind",jsonString (reuseVersionName kind)),("version",jsonString version)]
        Just (ReuseImage version) -> [("unit","null"),("module","null")
          ,("version_kind",jsonString "image_identity"),("version",jsonString version)]
      fields = [("schema","1"),("cycle",show cycleId),("purpose",jsonString purpose)
        ,("stage",jsonString (reuseStageName stage)),("decision",jsonString (reuseDecisionName decision))
        ,("reason",jsonString (reuseReasonName reason)),("items",show items)
        ,("bytes",maybe "null" show bytes),("observed_ns",show observed)] ++ moduleFields
  hPutStrLn stderr ("tidepool-reuse {" ++ intercalate ","
    [jsonString key ++ ":" ++ value | (key,value) <- fields] ++ "}")

-- Required even for zero work. Missing completion means unobserved, not zero.
emitReuseComplete :: Bool -> ReuseContext -> ReuseStage -> IO ()
emitReuseComplete enabled context stage =
  emitReuse enabled context stage ReuseComplete StageComplete Nothing 0 Nothing

-- A successful checked-environment cycle never enters native preparation,
-- site classification, original recovery or raw projection. This terminal
-- evidence describes applicability, not completed work or a cache decision.
emitCheckOnlyReuseApplicability :: Bool -> ReuseContext -> IO ()
emitCheckOnlyReuseApplicability enabled context =
  mapM_ (\stage -> emitReuse enabled context stage ReuseNotApplicable CheckOnlyStage Nothing 0 Nothing)
    [PreparedBody, SiteWitness, OriginalRecovery, RawProjection]

-- | Read the @TIDEPOOL_TIMING@ env var. On iff exactly @"1"@; unset or any
-- other value is off.
readTimingEnabled :: IO Bool
readTimingEnabled = (== Just "1") <$> lookupEnv "TIDEPOOL_TIMING"

-- | Run @act@ and, when @enabled@, write one
-- @tidepool-timing phase=\<name\> ms=\<int\>@ line to stderr AFTER @act@
-- completes, plus a nested resource detail when enabled. Disabled calls
-- retain wall timing internally and emit no diagnostic lines.
timePhase :: MonadIO m => Bool -> String -> m a -> m a
timePhase enabled name act = do
  (r, ms) <- timeSection (timeDetailPhase enabled "compile" name act)
  liftIO (emitPhase enabled name ms)
  pure r

-- | Measure a named child without putting it in the flat phase stream.
-- Enabled measurements include monotonic boundaries for perf alignment,
-- process CPU and RTS deltas. Reading counters never forces a collection;
-- allocation therefore follows the RTS accounting boundary, not a heap census.
timeDetailPhase :: MonadIO m => Bool -> String -> String -> m a -> m a
timeDetailPhase False _ _ act = act
timeDetailPhase True parent name act = do
  start <- beginResourceTiming True
  result <- act
  endResourceTiming start parent name
  pure result

-- | Bracket a task using its actual compiler owner. The enclosing transport
-- identifies the physical invocation; module identity distinguishes concurrent
-- tasks without interpreting logger delivery order as execution order.
timeModuleDetailPhase :: MonadIO m => Bool -> String -> String -> Module -> m a -> m a
timeModuleDetailPhase False _ _ _ act = act
timeModuleDetailPhase True parent name owner act = do
  start <- beginResourceTiming True
  result <- act
  endResourceTimingWithOwner start parent name (Just owner)
  pure result

-- A token brackets existing wall timers whose setup spans several statements.
-- Keep its constructor private so all resource spans use the same clock and
-- counter ordering. Nothing performs no counter reads when timing is disabled.
data ResourceTimingStart = ResourceTimingStart Word64 Integer (Maybe RTS.RTSStats)

beginResourceTiming :: MonadIO m => Bool -> m (Maybe ResourceTimingStart)
beginResourceTiming False = pure Nothing
beginResourceTiming True = do
  wall0 <- liftIO getMonotonicTimeNSec
  cpu0 <- liftIO getCPUTime
  rts0 <- liftIO readRtsStats
  pure (Just (ResourceTimingStart wall0 cpu0 rts0))

endResourceTiming :: MonadIO m => Maybe ResourceTimingStart -> String -> String -> m ()
endResourceTiming start parent name = endResourceTimingWithOwner start parent name Nothing

endResourceTimingWithOwner :: MonadIO m => Maybe ResourceTimingStart -> String -> String -> Maybe Module -> m ()
endResourceTimingWithOwner Nothing _ _ _ = pure ()
endResourceTimingWithOwner (Just (ResourceTimingStart wall0 cpu0 rts0)) parent name owner = do
  rts1 <- liftIO readRtsStats
  cpu1 <- liftIO getCPUTime
  wall1 <- liftIO getMonotonicTimeNSec
  liftIO $ hPutStrLn stderr ("tidepool-timing-detail parent=" ++ parent
    ++ " phase=" ++ name
    ++ " ms=" ++ show ((delta wall0 wall1 + 500000) `div` 1000000)
    ++ " start_ns=" ++ show wall0 ++ " end_ns=" ++ show wall1
    ++ " wall_ns=" ++ show (delta wall0 wall1)
    ++ " cpu_ns=" ++ show ((cpu1 - cpu0) `div` 1000)
    ++ maybe "" (\actual -> " owner_unit=" ++ unitString (moduleUnit actual)
      ++ " owner_module=" ++ moduleNameString (moduleName actual)) owner
    ++ renderRtsDelta rts0 rts1)

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
emitCount True name count = do
  -- Position the count event on the phase clock; this is not a duration of
  -- the counted work. Older lines without this field are request totals only.
  countNs <- getMonotonicTimeNSec
  hPutStrLn stderr ("tidepool-count name=" ++ name ++ " count=" ++ show count
    ++ " count_ns=" ++ show countNs)

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
emitCompileSummary :: Int -> Integer -> Integer -> Integer -> Integer -> Int -> Int -> Int
  -> [(String, Integer)] -> [(String, Integer)] -> IO ()
emitCompileSummary moduleCount wallMs typecheckMs loweringMs interfaceMs frontCount backCount interfaceCount topModules topInterfaces =
  hPutStrLn stderr $
    "tidepool-compile-summary modules=" ++ show moduleCount
    ++ " wall_ms=" ++ show wallMs
    ++ " typecheck_ms=" ++ show typecheckMs
    ++ " lowering_ms=" ++ show loweringMs
    ++ " interface_ms=" ++ show interfaceMs
    ++ " phase_coverage=deferred_modules"
    ++ " typecheck_modules=" ++ show frontCount
    ++ " lowering_modules=" ++ show backCount
    ++ " interface_modules=" ++ show interfaceCount
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
    ++ renderRtsDelta before after

renderRtsDelta :: Maybe RTS.RTSStats -> Maybe RTS.RTSStats -> String
renderRtsDelta before after = case (before, after) of
  (Just rts0, Just rts1) ->
    let collections = delta (RTS.gcs rts0) (RTS.gcs rts1)
        majorCollections = delta (RTS.major_gcs rts0) (RTS.major_gcs rts1)
    in " rts=enabled rts_scope=process_delta"
      ++ " allocated_bytes=" ++ show (delta (RTS.allocated_bytes rts0) (RTS.allocated_bytes rts1))
      ++ " gc_cpu_ns=" ++ show (delta (RTS.gc_cpu_ns rts0) (RTS.gc_cpu_ns rts1))
      ++ " gc_elapsed_ns=" ++ show (delta (RTS.gc_elapsed_ns rts0) (RTS.gc_elapsed_ns rts1))
      ++ " gcs=" ++ show collections
      ++ " major_gcs=" ++ show majorCollections
      ++ " minor_gcs=" ++ show (delta majorCollections collections)
      ++ renderRtsSnapshot "before" before ++ renderRtsSnapshot "after" after
      ++ renderRtsHighWater after
  _ ->
    " rts=unavailable rts_scope=process_delta"
      ++ " allocated_bytes=unavailable gc_cpu_ns=unavailable"
      ++ " gc_elapsed_ns=unavailable gcs=unavailable"
      ++ " major_gcs=unavailable minor_gcs=unavailable"
      ++ renderRtsSnapshot "before" before ++ renderRtsSnapshot "after" after
      ++ renderRtsHighWater after

-- These gauges describe the last completed GC, not the sampling instant.
-- Equal epochs expose stale samples; epoch zero has no GC details. Minor
-- collections count uncollected generations as live. No collection is forced.
renderRtsSnapshot :: String -> Maybe RTS.RTSStats -> String
renderRtsSnapshot suffix stats =
  renderRtsValue ("last_gc_epoch_" ++ suffix) (RTS.gcs <$> stats)
    ++ renderRtsValue ("last_gc_gen_" ++ suffix) (RTS.gcdetails_gen <$> details)
    ++ renderRtsValue ("last_gc_live_bytes_" ++ suffix) (RTS.gcdetails_live_bytes <$> details)
    ++ renderRtsValue ("last_gc_mem_in_use_bytes_" ++ suffix) (RTS.gcdetails_mem_in_use_bytes <$> details)
  where
    details = stats >>= \value ->
      if RTS.gcs value == 0 then Nothing else Just (RTS.gc value)

-- Process-lifetime maxima are not phase peaks or native RSS. The live maximum
-- is sampled only at major (oldest-generation) collections, including -G1.
renderRtsHighWater :: Maybe RTS.RTSStats -> String
renderRtsHighWater stats =
  renderRtsValue "process_highwater_major_gc_live_bytes" live
    ++ renderRtsValue "process_highwater_rts_mem_in_use_bytes" capacity
  where
    live = stats >>= \value ->
      if RTS.major_gcs value == 0 then Nothing else Just (RTS.max_live_bytes value)
    capacity = stats >>= \value ->
      if RTS.gcs value == 0 then Nothing else Just (RTS.max_mem_in_use_bytes value)

renderRtsValue :: Show a => String -> Maybe a -> String
renderRtsValue name value = " " ++ name ++ "=" ++ maybe "unavailable" show value

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

-- Selection observations describe the immutable ingress before the working
-- memo is narrowed. They do not grant reuse or search historical alternatives.
data MemoSelectionState = MemoOwnerAbsent | MemoIngressAbsent | MemoMatchingRejected
  | MemoSelected | MemoTargetExcluded | MemoStandalone deriving (Eq, Show)
data MemoSelectionTrace = MemoSelectionTrace
  { selectionState :: MemoSelectionState
  , selectionUnit :: String
  , selectionKeySha256 :: String
  , selectionNarrowedVersions :: Int
  , selectionOriginatingCycle :: Maybe Word64
  , selectionRejectedChecks :: [(Word64, [String])]
  , selectionRejectedOmitted :: Int
  , selectionExecutable :: Maybe MemoExecutableTrace
  }
data MemoExecutableTrace = MemoExecutableTrace
  { executablePreviousEpoch :: Maybe Word64
  , executableCurrentEpoch :: Word64
  , executableHasCode :: Bool
  , executableNeedsBytecode :: Bool
  }

-- | One line per module in the selected module graph, written once per
-- compile cycle (never per lookup) when @TIDEPOOL_MEMO_TRACE=1@. Graph
-- capture holds paths and fingerprints only, never source or Core.
emitMemoCycleGraph
  :: Bool -> Word64 -> String -> String -> Maybe FilePath -> Maybe FilePath
  -> Fingerprint -> [String] -> String -> MemoSelectionTrace -> IO ()
emitMemoCycleGraph False _ _ _ _ _ _ _ _ _ = pure ()
emitMemoCycleGraph True cycleId moduleName sourceKind selectedPath resolvedPath
  sourceFingerprint directDeps dependencyDigest selection =
  hPutStrLn stderr $
    "tidepool-memo-cycle-graph cycle=" ++ show cycleId
      ++ " module=" ++ moduleName
      ++ " source_kind=" ++ sourceKind
      ++ " selected_path=" ++ maybe "<none>" id selectedPath
      ++ " resolved_path=" ++ maybe "<none>" id resolvedPath
      ++ " fingerprint=" ++ show sourceFingerprint
      ++ " direct_deps=" ++ intercalate "," directDeps
      ++ " dependency_digest=" ++ dependencyDigest
      ++ " unit=" ++ show (selectionUnit selection)
      ++ " selection=" ++ show (selectionState selection)
      ++ " selection_key_sha256=" ++ selectionKeySha256 selection
      ++ " narrowed_versions=" ++ show (selectionNarrowedVersions selection)
      ++ " selected_originating_cycle=" ++ maybe "none" show (selectionOriginatingCycle selection)
      ++ " rejected_checks=" ++ show (selectionRejectedChecks selection)
      ++ " rejected_checks_omitted=" ++ show (selectionRejectedOmitted selection)
      ++ case selectionExecutable selection of
        Nothing -> " executable_observation=unknown"
        Just capacity -> " executable_observation=observed"
          ++ " previous_epoch=" ++ maybe "none" show (executablePreviousEpoch capacity)
          ++ " current_epoch=" ++ show (executableCurrentEpoch capacity)
          ++ " has_code_before_refresh=" ++ show (executableHasCode capacity)
          ++ " needs_bytecode=" ++ show (executableNeedsBytecode capacity)

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
