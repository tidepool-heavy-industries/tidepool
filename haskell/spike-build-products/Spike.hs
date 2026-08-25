{-# LANGUAGE ScopedTypeVariables #-}

-- | Build-products-dir spike, a prerequisite toward a resident compile
-- daemon: does GHC's OWN recompilation checking ('GHC.Iface.Recomp.checkOldIface',
-- reached through 'load'') actually SKIP an unchanged home module under
-- @backend = noBackend@ (what 'Tidepool.GhcPipeline.canonicalizeDFlags'
-- pins every extraction to) when interfaces are written to, and read back
-- from, a STABLE on-disk directory across two independent 'runGhc' sessions
-- (an empty-HPT session is the closest same-process proxy for "a fresh
-- @tidepool-extract@ process" — the real cross-process case this dir is
-- for)?
--
-- This is a go/no-go probe, not the feature: a clean RED (naming the
-- blocking mechanism) is exactly as valid a landing as a GREEN, per the lane
-- brief's SPIKE-GATE.
--
-- MECHANISM: 'extractionDynFlags'\/'canonicalizeDFlags' are lifted
-- (copied, not imported — both are un-exported from 'Tidepool.GhcPipeline',
-- and the lane boundary forbids editing that module before the gate opens)
-- byte-for-byte from GhcPipeline.hs, extended with 'withBuildProducts' to add
-- exactly the two fields + one flag a warm hidir needs: 'hiDir', 'objectDir',
-- 'Opt_WriteInterface'. The 'Messager' instrument (spikeMessager\/
-- describeRecomp) is lifted verbatim from spike-batch\/Spike.hs, which
-- already proved this pattern gives a DIRECT read of GHC's own per-module
-- recompile verdict (not a timing inference).
--
-- THREE CYCLES, each its OWN fresh 'runGhc' session (fresh HPT — no
-- in-process memoization possible, the only channel between cycles is the
-- shared on-disk 'bpDir'):
--
--   1. COLD: bpDir empty. Compile Target.hs (imports "Tidepool.Prelude" for
--      a real ~14-module stdlib closure) with @-fwrite-interface@ into
--      bpDir. Expect: every module NeedsRecompile (nothing to reuse yet).
--   2. WARM, IDENTICAL: same bpDir (now populated), same Target.hs content,
--      fresh session. Expect: every module UpToDate — the collapse this
--      lane needs.
--   3. WARM, TARGET CHANGED: same bpDir, Target.hs body edited (proves this
--      isn't a vacuous "always UpToDate" reading — a real content change
--      must still force a real recompile), fresh session. Expect: exactly
--      Target itself NeedsRecompile; every stdlib dependency stays
--      UpToDate — the production shape (a novel per-turn module, a fixed
--      stdlib closure).
module Main (main) where

import GHC
import GHC.Driver.Main (Messager, hscDesugar)
import GHC.Driver.Make (load')
import GHC.Driver.Env (hscUpdateFlags)
import GHC.Iface.Recomp (RecompileRequired(..), CompileReason(..))
import GHC.Types.Error (mkUnknownDiagnostic)
import GHC.Unit.Module.Graph (moduleGraphNodeModule)
import GHC.Driver.Session
  ( updOptLevel, gopt_set, gopt_unset
  , packageFlags, PackageFlag(..), PackageArg(..), ModRenaming(..) )
import GHC.Platform (genericPlatform)
import GHC.Utils.Outputable (renderWithContext, defaultSDocContext, ppr)

import GHC.Core (CoreBind, Bind(..), maybeUnfoldingTemplate)
import GHC.Core.Opt.Pipeline (core2core)
import GHC.Unit.Module.ModGuts (ModGuts(..))
import GHC.Types.Var (idInfo)
import GHC.Types.Id.Info (unfoldingInfo)
import GHC.Types.Name (getOccString)

import Data.IORef (IORef, newIORef, modifyIORef', readIORef)
import Control.Monad (forM_, forM, when)
import Control.Monad.IO.Class (liftIO)
import Control.Monad.Catch (try, SomeException)
import qualified Data.Set as Set
import Data.List (sort)
import System.Directory
  (createDirectoryIfMissing, removeDirectoryRecursive, doesDirectoryExist)
import System.Environment (lookupEnv)
import System.FilePath ((</>))
import System.Process (readProcess)
import Text.Printf (printf)
import System.Exit (exitFailure, exitSuccess)

import Tidepool.Timing (timeSection)

--------------------------------------------------------------------------------
-- Lifted from GhcPipeline.hs (NOT exported there; copied per the lane
-- boundary rather than modifying that module — see spike-batch/Spike.hs for
-- the same precedent). Byte-for-byte the same transform, comments trimmed.
--------------------------------------------------------------------------------

canonicalizeDFlags :: DynFlags -> DynFlags
canonicalizeDFlags dflags =
  (`gopt_set` Opt_UseBytecodeRatherThanObjects) $
  (`gopt_unset` Opt_ShowErrorContext) $
  gopt_set (gopt_set (gopt_unset (gopt_unset (updOptLevel 2 $ dflags
        { backend = noBackend
        , ghcLink = NoLink
        , maxRelevantBinds = Just 0
        }) Opt_FullLaziness) Opt_CprAnal)
        Opt_ExposeAllUnfoldings) Opt_ExposeOverloadedUnfoldings

extractionDynFlags :: DynFlags -> [FilePath] -> DynFlags
extractionDynFlags dflags includes = canonicalizeDFlags dflags
  { importPaths = importPaths dflags ++ includes
  , packageFlags = packageFlags dflags
      ++ [ExposePackage "-package ghc" (PackageArg "ghc")
                        (ModRenaming True [])]
  , targetPlatform = genericPlatform
  , sseVersion = Nothing
  , bmiVersion = Nothing
  , avx = False
  , avx2 = False
  , avx512cd = False
  , avx512er = False
  , avx512f = False
  , avx512pf = False
  }

-- | The ONE thing this spike adds on top of 'extractionDynFlags': point
-- interface (and, harmlessly under @noBackend@/@NoLink@, object) output at a
-- STABLE directory and turn on @-fwrite-interface@ so 'load'' actually
-- persists what it typechecks — today's production 'extractionDynFlags'
-- does neither, which is exactly why nothing survives one extract spawn to
-- the next (see the lane brief's prerequisite (1)).
withBuildProducts :: FilePath -> DynFlags -> DynFlags
withBuildProducts bpDir dflags = (`gopt_set` Opt_WriteInterface) dflags
  { hiDir = Just bpDir
  , objectDir = Just bpDir
  }

getLibdir :: IO FilePath
getLibdir = do
  envDir <- lookupEnv "TIDEPOOL_GHC_LIBDIR"
  case envDir of
    Just dir -> pure dir
    Nothing  -> trim <$> readProcess "ghc" ["--print-libdir"] ""
  where trim = reverse . dropWhile (== '\n') . reverse

--------------------------------------------------------------------------------
-- THE INSTRUMENT: a Messager recording GHC's own per-module recompile
-- verdict — lifted verbatim from spike-batch/Spike.hs, which already proved
-- this is a direct observation (not a timing inference) of the question.
--------------------------------------------------------------------------------

describeRecomp :: RecompileRequired -> String
describeRecomp UpToDate = "UpToDate"
describeRecomp (NeedsRecompile MustCompile) = "NeedsRecompile(MustCompile)"
describeRecomp (NeedsRecompile (RecompBecause reason)) =
  "NeedsRecompile(" ++ renderWithContext defaultSDocContext (ppr reason) ++ ")"

spikeMessager :: IORef [(String, String)] -> Messager
spikeMessager ref _hsc _idx recomp node =
  case moduleGraphNodeModule node of
    Just mn -> modifyIORef' ref ((moduleNameString mn, describeRecomp recomp) :)
    Nothing -> pure ()

isRecompiled :: String -> Bool
isRecompiled verdict = take 14 verdict == "NeedsRecompile"

--------------------------------------------------------------------------------
-- Fixture layout
--------------------------------------------------------------------------------

workDir :: FilePath
workDir = "spike-build-products/work"

srcDir, bpDir :: FilePath
srcDir = workDir </> "src"
bpDir  = workDir </> "bp"

targetSrc :: String -> String
targetSrc body = unlines
  [ "{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}"
  , "module Target where"
  , "import Tidepool.Prelude"
  , ""
  , "greet :: Text -> Text"
  , "greet name = " ++ body
  ]

bodyA, bodyB :: String
bodyA = "\"hello \" <> toUpper name"
bodyB = "\"howdy \" <> toUpper name <> \"!\""

--------------------------------------------------------------------------------
-- One cycle: a FRESH 'runGhc' session (fresh HPT — the only channel to a
-- prior cycle is the shared 'bpDir' on disk), 'depanal' + 'load'' with the
-- Messager instrument threaded through.
--------------------------------------------------------------------------------

data CycleResult = CycleResult
  { crLabel      :: String
  , crRecompiles :: [(String, String)] -- ^ (module name, verdict), load order
  , crLoadMs     :: Integer
  , crFailure    :: Maybe String
  }

runCycle :: String -> Ghc CycleResult
runCycle label = do
  recompRef <- liftIO (newIORef [])
  attempt <- try $ do
    target <- guessTarget (srcDir </> "Target.hs") Nothing Nothing
    setTargets [target]
    modGraph <- depanal [] False
    load' Nothing LoadAllTargets mkUnknownDiagnostic (Just (spikeMessager recompRef)) modGraph
  recomps <- liftIO (reverse <$> readIORef recompRef)
  case attempt of
    Left (e :: SomeException) -> pure (CycleResult label recomps 0 (Just (show e)))
    Right loadFlag -> case loadFlag of
      Failed    -> pure (CycleResult label recomps 0 (Just "load' reported Failed"))
      Succeeded -> pure (CycleResult label recomps 0 Nothing)

-- | Run one cycle in its OWN fresh 'runGhc' session — the same-process proxy
-- for "a fresh tidepool-extract process" (see module haddock). 'crLoadMs' is
-- the WHOLE session's setSessionDynFlags+depanal+load' wall-clock (not
-- load' alone) — a cheap, honest cycle-cost proxy for this probe, distinct
-- from production's own per-phase 'emitPhase' timing.
runFreshCycle :: FilePath -> String -> IO CycleResult
runFreshCycle libdir label = do
  (result, cycleMs) <- timeSection $ runGhc (Just libdir) $ do
    dflags <- getSessionDynFlags
    _ <- setSessionDynFlags (withBuildProducts bpDir (extractionDynFlags dflags [srcDir, "lib"]))
    runCycle label
  pure result { crLoadMs = cycleMs }

printCycle :: CycleResult -> IO ()
printCycle r = do
  printf "\n--- cycle: %s ---\n" (crLabel r)
  case crFailure r of
    Just e -> putStrLn ("  FAILED: " ++ unwords (words e))
    Nothing -> do
      forM_ (crRecompiles r) $ \(m, verdict) -> printf "  %-28s %s\n" m verdict
      let n = length (crRecompiles r)
          recompiled = length (filter (isRecompiled . snd) (crRecompiles r))
      printf "  load' wall-clock: %d ms\n" (crLoadMs r)
      printf "  compile summary: modules=%d recompiled=%d skipped=%d\n" n recompiled (n - recompiled)

--------------------------------------------------------------------------------
-- FOLLOW-UP (not part of the go/no-go gate above): the lane brief's
-- prerequisite (3) — "the pipeline's manual second pass must reuse the
-- load'' results rather than re-typechecking, else the cache only touches
-- the ~60% upsweep share". A SKIPPED module (checkOldIface says UpToDate)
-- never runs 'GhcPipeline.hs''s own compileFront\/compileBack; the manual
-- pass would need that module's Core from SOMEWHERE else. GHC already
-- reconstructs each home module's typechecked 'TypeEnv' from a skipped
-- interface (proven indirectly by the 3-cycle gate above: cycles 2\/3's
-- 'Target' correctly resolves 'Tidepool.Prelude' names it never
-- recompiled), and — because 'canonicalizeDFlags' turns on
-- 'Opt_ExposeAllUnfoldings'\/'Opt_ExposeOverloadedUnfoldings' — every
-- exported binding's interface carries a real unfolding for cross-module
-- inlining. This probe asks directly: is that unfolding a COMPLETE 'CoreExpr'
-- template for EVERY top-level binder a fresh compile produces, or only for
-- some (small/inlinable) subset? Compares 'Tidepool.Prelude''s own top-level
-- binder OccName set from a FRESH compile (ground truth: 'core2core'-
-- simplified 'mg_binds') against the SAME module's binder set reconstructed
-- via 'maybeUnfoldingTemplate' after a SKIPPED load from the warm bpDir this
-- spike already populated.
--------------------------------------------------------------------------------

bindNames :: CoreBind -> [String]
bindNames (NonRec b _) = [getOccString b]
bindNames (Rec ps)     = [getOccString b | (b, _) <- ps]

-- | Fresh-compile ONE module by path, no warm bpDir involved — the
-- ground-truth top-level binder OccName set its own 'core2core'-simplified
-- 'ModGuts' carries.
freshModuleBinderNames :: FilePath -> String -> Ghc (Either String [String])
freshModuleBinderNames path modNameStr = do
  target <- guessTarget path Nothing Nothing
  setTargets [target]
  modGraph <- depanal [] False
  loadFlag <- load' Nothing LoadAllTargets mkUnknownDiagnostic Nothing modGraph
  case loadFlag of
    Failed -> pure (Left "load' failed")
    Succeeded -> do
      summaries <- mgModSummaries <$> getModuleGraph
      case [ ms | ms <- summaries, moduleNameString (moduleName (ms_mod ms)) == modNameStr ] of
        (modSum0 : _) -> do
          let modSum = modSum0 { ms_hspp_opts = canonicalizeDFlags (ms_hspp_opts modSum0) }
          typechecked <- typecheckModule =<< parseModule modSum
          hscEnv0 <- getSession
          let hscEnv   = hscUpdateFlags canonicalizeDFlags hscEnv0
              tcGblEnv = fst (tm_internals_ typechecked)
          desugared  <- liftIO (hscDesugar hscEnv modSum tcGblEnv)
          simplified <- liftIO (core2core hscEnv desugared)
          pure (Right (sort (concatMap bindNames (mg_binds simplified))))
        [] -> pure (Left "module not found in graph")

-- | SKIP-load the module from the warm bpDir (no re-typecheck), then pull
-- its top-level binder set back out via 'getModuleInfo' \/
-- 'modInfoTyThings' — for each 'Id', 'maybeUnfoldingTemplate' on its
-- 'unfoldingInfo' yields 'Just' a real 'CoreExpr' iff the loaded interface
-- carried a usable unfolding for it. Only those names are counted.
skippedModuleUnfoldingNames :: FilePath -> String -> Ghc (Either String [String])
skippedModuleUnfoldingNames path modNameStr = do
  target <- guessTarget path Nothing Nothing
  setTargets [target]
  modGraph <- depanal [] False
  loadFlag <- load' Nothing LoadAllTargets mkUnknownDiagnostic Nothing modGraph
  case loadFlag of
    Failed -> pure (Left "load' failed")
    Succeeded -> do
      summaries <- mgModSummaries <$> getModuleGraph
      case [ ms_mod ms | ms <- summaries, moduleNameString (moduleName (ms_mod ms)) == modNameStr ] of
        (m : _) -> do
          mInfo <- getModuleInfo m
          case mInfo of
            Nothing -> pure (Left "getModuleInfo returned Nothing")
            Just info -> do
              let ids = [ i | AnId i <- modInfoTyThings info ]
                  named = [ (getOccString i, maybeUnfoldingTemplate (unfoldingInfo (idInfo i))) | i <- ids ]
              pure (Right (sort [ n | (n, Just _) <- named ]))
        [] -> pure (Left "module not found in graph")

runCoreReuseFollowUp :: FilePath -> IO ()
runCoreReuseFollowUp libdir = do
  putStrLn "\n################ FOLLOW-UP: Core-from-skipped-iface reconstruction ################"
  let preludePath = "lib" </> "Tidepool" </> "Prelude.hs"
  freshResult <- runGhc (Just libdir) $ do
    dflags <- getSessionDynFlags
    _ <- setSessionDynFlags (extractionDynFlags dflags ["lib"])
    freshModuleBinderNames preludePath "Tidepool.Prelude"
  skipResult <- runGhc (Just libdir) $ do
    dflags <- getSessionDynFlags
    _ <- setSessionDynFlags (withBuildProducts bpDir (extractionDynFlags dflags ["lib"]))
    skippedModuleUnfoldingNames preludePath "Tidepool.Prelude"
  case (freshResult, skipResult) of
    (Right freshNames, Right skipNames) -> do
      let freshSet = Set.fromList freshNames
          skipSet  = Set.fromList skipNames
          missing  = Set.difference freshSet skipSet
          extra    = Set.difference skipSet freshSet
      printf "  fresh top-level binders:                     %d\n" (Set.size freshSet)
      printf "  skip-loaded reconstructable-unfolding binders: %d\n" (Set.size skipSet)
      printf "  missing (fresh has, skip-load does NOT):     %d %s\n" (Set.size missing) (show (Set.toList missing))
      printf "  extra   (skip-load has, fresh does NOT):     %d %s\n" (Set.size extra) (show (Set.toList extra))
      putStrLn $ if Set.null missing
        then "  CORE-REUSE CANDIDATE: every fresh top-level binder has a reconstructable unfolding once loaded from a skipped interface."
        else "  CORE-REUSE BLOCKED: some top-level binders have NO usable unfolding once loaded from a skipped interface (see 'missing') -- the manual second pass cannot skip these via this mechanism without a different fix (e.g. forcing unfoldings for ALL bindings, not just exported/inlinable ones)."
    (l, r) -> printf "  FOLLOW-UP FAILED to run cleanly: fresh=%s skip=%s\n"
      (either id (const "ok") l) (either id (const "ok") r)

main :: IO ()
main = do
  libdir <- getLibdir
  exists <- doesDirectoryExist workDir
  when exists (removeDirectoryRecursive workDir)
  createDirectoryIfMissing True srcDir
  createDirectoryIfMissing True bpDir
  writeFile (srcDir </> "Target.hs") (targetSrc bodyA)

  putStrLn "################ SPIKE: warm build-products dir under noBackend ################"

  cold <- runFreshCycle libdir "1-cold (bpDir empty)"
  printCycle cold

  warmSame <- runFreshCycle libdir "2-warm, Target.hs UNCHANGED"
  printCycle warmSame

  writeFile (srcDir </> "Target.hs") (targetSrc bodyB)
  warmChanged <- runFreshCycle libdir "3-warm, Target.hs CHANGED"
  printCycle warmChanged

  putStrLn "\n================ VERDICT ================"
  let ranCleanly = all ((== Nothing) . crFailure) [cold, warmSame, warmChanged]
      coldAllRecompiled =
        not (null (crRecompiles cold)) && all (isRecompiled . snd) (crRecompiles cold)
      warmSameAllSkipped =
        not (null (crRecompiles warmSame)) && all (not . isRecompiled . snd) (crRecompiles warmSame)
      changedMods = [ m | (m, v) <- crRecompiles warmChanged, isRecompiled v ]
      warmChangedOnlyTarget = changedMods == ["Target"]
      go = ranCleanly && coldAllRecompiled && warmSameAllSkipped && warmChangedOnlyTarget
  printf "  ran cleanly:                         %s\n" (show ranCleanly)
  printf "  cycle 1 (cold) all NeedsRecompile:    %s\n" (show coldAllRecompiled)
  printf "  cycle 2 (warm, identical) all UpToDate: %s\n" (show warmSameAllSkipped)
  printf "  cycle 3 (warm, changed) ONLY Target recompiled: %s (recompiled=%s)\n"
    (show warmChangedOnlyTarget) (show changedMods)
  putStrLn $ if go
    then "  GREEN: checkOldIface, under backend=noBackend, SKIPS an unchanged home module across independent GHC sessions when interfaces are written to (-fwrite-interface) and read back from a stable on-disk hidir/objectdir. A real content change on the target still forces exactly that module to recompile while every stdlib dependency stays skipped."
    else "  RED: see the per-cycle detail above for the blocking mechanism."

  runCoreReuseFollowUp libdir

  if go then exitSuccess else exitFailure
