{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE LambdaCase #-}

-- | batch-turns feasibility spike
-- (plans/post-restart/batch-turns-feasibility.md §2.3): the ONE open gate —
-- can a SINGLE 'runGhc' session run N sequential
-- setTargets\/depanal\/load' cycles with items 2..N skipping the stdlib
-- @load'@? Go\/no-go probe only, not the feature. A clean RED (naming the
-- blocking mechanism) is exactly as valid a landing as a GREEN.
--
-- MECHANISM: everything GHC-session-plumbing below is LIFTED (copied, not
-- imported) from 'Tidepool.GhcPipeline.runCompile' + its @sessionVariant@ —
-- the depanal-exclude/load'/mod-graph-restore/inject dance, and the
-- interleaved parse\/typecheck\/desugar\/core2core per-module loop with
-- deferred-module HPT registration. That function is a single un-exported
-- @IO ()@-shaped skeleton, not something this spike can reuse by import, and
-- the lane brief forbids modifying GhcPipeline.hs\/Session.hs\/app\/Main.hs to
-- export more of it — so the loop is copied here, trimmed of the tier
-- machinery (E6, warning capture, hs-boot exclusion accounting) this spike
-- does not need. 'Tidepool.Session' (mkThinSessionIface\/writeSessionIface\/
-- injectSessionScope) and 'Tidepool.GhcPipeline.stripMonadHead' ARE exported,
-- production surface, and used here unmodified.
--
-- THE INSTRUMENT: a caller-supplied 'Messager' passed to 'load'' records, per
-- (cycle, module), GHC's OWN recompilation verdict — UpToDate vs
-- NeedsRecompile \<reason\> — for every module 'load'' actually visits. That
-- is a DIRECT observation of the question ("does load' see the already-HPT
-- stdlib as up to date on cycle 2..N"), not a timing inference. Wall-clock
-- for 'load'' and for the per-module compile loop is measured too, but the
-- Messager table is the evidence.
--
-- CHAIN: cycle k compiles @InputK.hs@ (importing @Tidepool.Prelude@ for real
-- stdlib compile cost) to a @__result@ binding; the compiled type is
-- captured, synthesized into a thin @Tidepool.Session.Val.Gk@ iface via the
-- PRODUCTION 'mkThinSessionIface'\/'writeSessionIface', and injected before
-- the NEXT cycle so @Input(k+1).hs@ can @import Tidepool.Session.Val.Gk (v)@
-- and reference @v@ in its own @__result@ — proving (or disproving) that a
-- later cycle's typecheck resolves an earlier cycle's injected binder, in
-- addition to the load' recompile question.
module Main (main) where

import GHC
import GHC.Driver.Main (hscDesugar, hscTidy, Messager)
import GHC.Driver.Env (hscUpdateFlags, hscUpdateHPT)
import GHC.Driver.Env.Types (HscEnv(hsc_mod_graph))
import GHC.Unit.Home.ModInfo (HomeModInfo(..), emptyHomeModInfoLinkable, addToHpt)
import GHC.Driver.Make (load', ModIfaceCache, newIfaceCache)
import GHC.Iface.Make (mkIfaceTc)
import GHC.Iface.Recomp (RecompileRequired(..), CompileReason(..))
import GHC.Types.SafeHaskell (SafeHaskellMode(Sf_None))
import GHC.Types.SourceFile (HscSource(..))
import GHC.Types.Error (mkUnknownDiagnostic)
import GHC.Types.SrcLoc (unLoc)
import GHC.Unit.Module.Graph
  ( mapMG, mkModuleGraph, mgModSummaries', ModuleGraphNode(..), moduleGraphNodeModule )
import GHC.Data.Graph.Directed (flattenSCCs)
import GHC.Core.Opt.Pipeline (core2core)
import GHC.Driver.Session
  ( updOptLevel, gopt_set, gopt_unset
  , packageFlags, PackageFlag(..), PackageArg(..), ModRenaming(..) )
import GHC.Unit.Module.ModGuts (CgGuts(..))
import GHC.Platform (genericPlatform)
import GHC.Utils.Outputable (renderWithContext, defaultSDocContext, ppr)
import GHC.Types.Id (idName, idType)
import GHC.Core.Type (Type)
import GHC.Types.TypeEnv (typeEnvIds)
import GHC.Tc.Types (TcGblEnv, tcg_type_env)
import GHC.Types.Name (nameOccName)
import GHC.Types.Name.Occurrence (mkVarOcc, occNameString)

import qualified Data.Set as Set
import Data.IORef (IORef, newIORef, modifyIORef', readIORef)
import Control.Monad (forM, forM_, when, (>=>))
import Control.Monad.IO.Class (liftIO)
import Control.Monad.Catch (try, SomeException)
import System.Directory (createDirectoryIfMissing, removeDirectoryRecursive, doesDirectoryExist)
import System.Environment (lookupEnv)
import System.FilePath (takeBaseName, (</>))
import System.Process (readProcess)
import Data.Char (toUpper)
import Text.Printf (printf)
import System.Exit (exitFailure, exitSuccess)

import Tidepool.Session
  ( Generation(..), SessionModuleKind(ValMod), SessionModule(..)
  , SessionScope(..), renderSessionModule
  , mkThinSessionIface, writeSessionIface, injectSessionScope )
import Tidepool.GhcPipeline (stripMonadHead)
import Tidepool.Timing (timeSection)

--------------------------------------------------------------------------------
-- Fixture layout
--------------------------------------------------------------------------------

-- | Scratch dir for one scenario's generated turn modules AND session .hi
-- files (co-located; 'injectSessionIface' reads by raw path, not via search
-- path, so this is fine). Mirrors 'test-session/work' precedent. Tagged per
-- scenario so the two scenarios in 'main' (no cache \/ with 'ModIfaceCache')
-- never share filesystem state.
workDir :: String -> FilePath
workDir tag = "spike-batch/work-" ++ tag

resultBinder :: String
resultBinder = "__result"

-- | The 3-cycle turn-module chain. Cycle k imports cycle (k-1)'s injected
-- @Val.G<k-1>@ and references its @v@ binder inside its own @__result@ — the
-- load-bearing reference this spike must prove resolves (or doesn't).
inputSrc :: Int -> String
inputSrc 1 = unlines
  [ "{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}"
  , "module Input1 where"
  , "import Tidepool.Prelude"
  , ""
  , "__result :: Text"
  , "__result = toUpper \"hello\""
  ]
inputSrc k = unlines
  [ "{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}"
  , "module Input" ++ show k ++ " where"
  , "import Tidepool.Prelude"
  , "import Tidepool.Session.Val.G" ++ show (k - 1) ++ " (v)"
  , ""
  , "__result :: Text"
  , "__result = v <> \"_c" ++ show k ++ "\""
  ]

--------------------------------------------------------------------------------
-- Lifted from GhcPipeline.hs (NOT exported there; copied per the lane
-- boundary rather than modifying that module). Byte-for-byte the same
-- transform, comments trimmed.
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

-- | Lifted from GhcPipeline.hs's 'capturedBindingType' (not exported there).
capturedBindingType :: String -> TcGblEnv -> Maybe Type
capturedBindingType occ tcg =
  case [ i | i <- typeEnvIds (tcg_type_env tcg)
           , occNameString (nameOccName (idName i)) == occ ] of
    (i:_) -> Just (idType i)
    []    -> Nothing

capitalize :: String -> String
capitalize [] = []
capitalize (c:cs) = toUpper c : cs

getLibdir :: IO FilePath
getLibdir = do
  envDir <- lookupEnv "TIDEPOOL_GHC_LIBDIR"
  case envDir of
    Just dir -> pure dir
    Nothing  -> trim <$> readProcess "ghc" ["--print-libdir"] ""
  where trim = reverse . dropWhile (== '\n') . reverse

--------------------------------------------------------------------------------
-- THE INSTRUMENT: a Messager recording GHC's own per-module recompile verdict
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

--------------------------------------------------------------------------------
-- One cycle
--------------------------------------------------------------------------------

data CycleReport = CycleReport
  { crCycle         :: Int
  , crTargetModule  :: String
  , crResolvedPrior :: Maybe Bool
    -- ^ 'Nothing' on cycle 1 (nothing prior to resolve); 'Just True'\/'Just
    -- False' on cycles 2.. per whether the cycle (whose target module
    -- references the prior cycle's injected binder) compiled successfully.
  , crRecompiles    :: [(String, String)]
    -- ^ (module name, load'-reported verdict), in load order.
  , crLoadMs        :: Integer
  , crCompileLoopMs :: Integer
  , crFailure       :: Maybe String
  }

-- | One setTargets\/depanal\/load'\/compile cycle, lifting the mechanism from
-- 'sessionVariant' in GhcPipeline.hs. Exceptions are caught HERE (not let
-- propagate out of 'main') so a RED cycle still yields a report instead of
-- crashing the whole probe uninformatively.
runCycle :: FilePath -> Maybe ModIfaceCache -> Int -> [SessionModule] -> Ghc (CycleReport, [SessionModule])
runCycle wd mIfaceCache k priorVals = do
  let path          = wd </> ("Input" ++ show k ++ ".hs")
      modName       = capitalize (takeBaseName path)
      targetModName' = mkModuleName modName
      excludedVal   = map renderSessionModule priorVals
  attempt <- try $ do
    target <- guessTarget path Nothing Nothing
    setTargets [target]
    modGraphRaw <- depanal excludedVal False
    let directSummaries = [ ms | ModuleNode _ ms <- mgModSummaries' modGraphRaw ]
        importsOf ms = [ unLoc lmn | (_, lmn) <- ms_textual_imps ms ]
        -- Everything that (transitively) imports a deferred module can't go
        -- through the load' below either — see sessionVariant's identical
        -- closure (GhcPipeline.hs). With one module per cycle this closes to
        -- just {target}, but the fixpoint is copied verbatim for fidelity.
        closure seed =
          let grown = seed `Set.union` Set.fromList
                [ ms_mod_name ms
                | ms <- directSummaries
                , any (`Set.member` seed) (importsOf ms) ]
          in if grown == seed then seed else closure grown
        deferredMods = closure (Set.fromList (targetModName' : excludedVal))
        depGraph = mkModuleGraph
          [ node | node <- mgModSummaries' modGraphRaw
                 , case node of
                     ModuleNode _ ms -> not (ms_mod_name ms `Set.member` deferredMods)
                     _               -> True ]
        unpoison ms =
          ms { ms_hspp_opts = gopt_unset (ms_hspp_opts ms) Opt_IgnoreInterfacePragmas }
    recompRef <- liftIO (newIORef [])
    (loadFlag, loadMs) <- timeSection $
      load' mIfaceCache LoadAllTargets mkUnknownDiagnostic (Just (spikeMessager recompRef))
            (mapMG unpoison depGraph)
    case loadFlag of
      Failed    -> liftIO $ ioError $ userError $
        "spike-batch cycle " ++ show k ++ ": PHASE 1 dependency load failed"
      Succeeded -> pure ()
    -- Restore the FULL module graph (target included), exactly as
    -- sessionVariant's cpAfterLoad does, so the per-module typecheck below
    -- can see HPT instances from dep modules.
    do hscMG <- getSession
       setSession hscMG { hsc_mod_graph = modGraphRaw }
    -- Inject the prior cycles' Val ifaces into the now dep-populated HPT.
    hsc0 <- getSession
    hscInjected <- injectSessionScope (SessionScope wd priorVals) hsc0
    setSession hscInjected
    let summaries =
          [ ms | ModuleNode _ ms <- flattenSCCs (topSortModuleGraph True modGraphRaw Nothing)
               , ms_hsc_src ms == HsSrcFile ]
        -- The ONE per-module front half (OptimizeEveryModule tier): parse,
        -- typecheck, capture __result's type, desugar. Lifted from
        -- GhcPipeline.hs's compileFront, trimmed of the tier/timing-phase
        -- bookkeeping this spike does not need.
        compileFront modSum0 = do
          let modSum = modSum0 { ms_hspp_opts = canonicalizeDFlags (ms_hspp_opts modSum0) }
          parsed      <- parseModule modSum
          typechecked <- typecheckModule parsed
          hscEnv0 <- getSession
          let hscEnv   = hscUpdateFlags canonicalizeDFlags hscEnv0
              tcGblEnv = fst (tm_internals_ typechecked)
              mResTy   = capturedBindingType resultBinder tcGblEnv
          desugared <- liftIO (hscDesugar hscEnv modSum tcGblEnv)
          pure (modSum, hscEnv, tcGblEnv, desugared, mResTy)
        -- The ONE per-module back half: core2core, then (for a deferred
        -- module only) register its tidied iface back into the HPT with
        -- emptyHomeModInfoLinkable — the exact mechanism risk (1) in the
        -- feasibility doc names, mirrored from GhcPipeline.hs's
        -- cpAfterModule.
        compileBack (modSum, hscEnv, tcGblEnv, desugared, mResTy) = do
          simplified <- liftIO (core2core hscEnv desugared)
          when (ms_mod_name modSum `Set.member` deferredMods) $ do
            (cgGuts, modDetails) <- liftIO (hscTidy hscEnv simplified)
            iface <- liftIO $
              mkIfaceTc hscEnv Sf_None modDetails modSum (Just (cg_binds cgGuts)) tcGblEnv
            let hmi = HomeModInfo iface modDetails emptyHomeModInfoLinkable
            hscEnvNow <- getSession
            setSession (hscUpdateHPT (\hpt -> addToHpt hpt (ms_mod_name modSum) hmi) hscEnvNow)
          pure (ms_mod_name modSum, mResTy)
    (results, compileMs) <- timeSection $ forM summaries (compileFront >=> compileBack)
    recomps <- liftIO (reverse <$> readIORef recompRef)
    resultTy <- case lookup targetModName' results of
      Just (Just ty) -> pure ty
      _ -> liftIO $ ioError $ userError $
        "spike-batch cycle " ++ show k ++ ": no " ++ resultBinder
        ++ " binder captured for " ++ modName
    let bound = stripMonadHead resultTy
        sm    = SessionModule ValMod (Generation (fromIntegral k))
    hscFinal <- getSession
    iface <- liftIO (mkThinSessionIface hscFinal sm [(mkVarOcc "v", bound)])
    liftIO (writeSessionIface hscFinal wd sm iface)
    pure (recomps, loadMs, compileMs)
  case attempt of
    Left (e :: SomeException) ->
      pure ( CycleReport k modName (if k == 1 then Nothing else Just False) [] 0 0 (Just (show e))
           , priorVals )
    Right (recomps, loadMs, compileMs) ->
      pure ( CycleReport k modName (if k == 1 then Nothing else Just True) recomps loadMs compileMs Nothing
           , priorVals ++ [SessionModule ValMod (Generation (fromIntegral k))] )

--------------------------------------------------------------------------------
-- Driver
--------------------------------------------------------------------------------

-- | Run cycles in order inside the SAME 'Ghc' session, stopping (but still
-- reporting) at the first failure — a mid-chain failure means later cycles
-- have no injected binder to build on anyway.
goCycles :: FilePath -> Maybe ModIfaceCache -> [Int] -> [SessionModule] -> Ghc [CycleReport]
goCycles _ _ [] _ = pure []
goCycles wd mIfaceCache (k:ks) priorVals = do
  (report, priorVals') <- runCycle wd mIfaceCache k priorVals
  case crFailure report of
    Just _  -> pure [report]
    Nothing -> (report :) <$> goCycles wd mIfaceCache ks priorVals'

isRecompiled :: String -> Bool
isRecompiled verdict = take 14 verdict == "NeedsRecompile"

-- | Print one scenario's per-cycle table and return whether it went GREEN
-- (cycles 2.. resolved the prior binder AND every load' entry was UpToDate).
printReport :: String -> [CycleReport] -> IO Bool
printReport label reports = do
  putStrLn ("\n################ SCENARIO: " ++ label ++ " ################")
  putStrLn "\n================ per-(cycle,module) load' recompile verdicts ================"
  forM_ reports $ \r -> do
    printf "\n--- cycle %d  target=%s  resolvedPriorBinder=%s ---\n"
      (crCycle r) (crTargetModule r) (show (crResolvedPrior r))
    case crFailure r of
      Just e  -> putStrLn ("  FAILED: " ++ oneLine e)
      Nothing -> do
        forM_ (crRecompiles r) $ \(m, verdict) -> printf "  %-42s %s\n" m verdict
        printf "  load' wall-clock:        %d ms\n" (crLoadMs r)
        printf "  compile-loop wall-clock: %d ms\n" (crCompileLoopMs r)
  putStrLn "\n================ VERDICT ================"
  forM_ reports $ \r ->
    let n = length (crRecompiles r)
        recompiled = length (filter (isRecompiled . snd) (crRecompiles r))
    in printf "  cycle %d: %d/%d load' entries NeedsRecompile%s\n"
         (crCycle r) recompiled n (if crFailure r /= Nothing then "  (cycle FAILED)" else "")
  let laterCycles = [ r | r <- reports, crCycle r >= 2 ]
      allSucceeded = length reports == 3 && all ((== Nothing) . crFailure) reports
      allResolved  = all ((== Just True) . crResolvedPrior) laterCycles
      allSkipped   = not (null laterCycles)
                  && all (\r -> not (any (isRecompiled . snd) (crRecompiles r))) laterCycles
      go = allSucceeded && allResolved && allSkipped
  putStrLn $ if go
    then "  GREEN: cycles 2.. resolved the prior binder AND load' reported every stdlib dependency UpToDate."
    else "  RED: see the per-cycle detail above for the blocking mechanism."
  pure go
  where oneLine = unwords . words

-- | Run the full 3-cycle chain in one fresh 'runGhc' session, under the
-- given scenario tag (its own scratch dir) and 'ModIfaceCache' choice.
runScenario :: FilePath -> String -> Maybe ModIfaceCache -> IO [CycleReport]
runScenario libdir tag mIfaceCache = do
  let wd = workDir tag
  exists <- doesDirectoryExist wd
  when exists (removeDirectoryRecursive wd)
  createDirectoryIfMissing True wd
  forM_ [1, 2, 3] $ \k -> writeFile (wd </> ("Input" ++ show k ++ ".hs")) (inputSrc k)
  runGhc (Just libdir) $ do
    dflags <- getSessionDynFlags
    _ <- setSessionDynFlags (extractionDynFlags dflags [wd, "lib"])
    goCycles wd mIfaceCache [1, 2, 3] []

-- | Two scenarios, both inside ONE 'runGhc' session each (the actual §2.3
-- question — N cycles, one session):
--
--   A. no 'ModIfaceCache' — byte-for-byte what GhcPipeline.hs's 'runCompile'
--      passes to every 'load'' call today (@load' Nothing ...@). THIS is the
--      answer to the feasibility gate as posed; its GREEN\/RED decides the
--      exit code.
--   B. a live 'ModIfaceCache', created once and threaded across all three
--      cycles — the ONE variation this spike tries per the lane brief's
--      "state whether any variation changes the answer" instruction, probing
--      whether the (undocumented in the feasibility doc) reason for A's
--      verdict has a cheap fix.
main :: IO ()
main = do
  libdir <- getLibdir
  reportsA <- runScenario libdir "noCache" Nothing
  goA <- printReport "A: no ModIfaceCache (matches production `load' Nothing ...`)" reportsA
  cache <- newIfaceCache
  reportsB <- runScenario libdir "withCache" (Just cache)
  _goB <- printReport "B: WITH a live ModIfaceCache threaded across all 3 cycles" reportsB
  if goA then exitSuccess else exitFailure
