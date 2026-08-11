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
import GHC.Unit.Module.ModGuts (ModGuts(..), CgGuts(..))
import GHC.Platform (genericPlatform)
import GHC.Utils.Outputable (renderWithContext, defaultSDocContext, ppr)
import GHC.Types.Id (idName, idType)
import GHC.Core.Type (Type)
import GHC.Types.TypeEnv (typeEnvIds)
import GHC.Tc.Types (TcGblEnv, tcg_type_env)
import GHC.Types.Name (nameOccName, nameUnique, mkExternalName, nameModule_maybe, nameSrcSpan)
import GHC.Types.Name.Occurrence (mkVarOcc, mkOccName, occNameSpace, occNameString)
-- The following four are lifted (per the lane boundary) so this spike can
-- rebuild GhcPipeline.hs's un-exported 'externalizeInternalTops' verbatim —
-- see the copy below, right before 'compileOneModule'.
import GHC.Core (CoreBind, Bind(..), Expr(..), Alt(..))
import GHC.Types.Var (setVarName)
import GHC.Types.Var.Env (mkVarEnv, lookupVarEnv)
import GHC.Types.Unique (getKey)

import qualified Data.Set as Set
import qualified Data.Map.Strict as Map
import qualified Data.Sequence as Seq
import Data.Word (Word64)
import Data.Text (Text)
import Data.Maybe (fromMaybe)
import Data.List (foldl')
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
import Tidepool.Translate (translateModuleClosed, ClosedModule(..), UnresolvedVar(..))

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

--------------------------------------------------------------------------------
-- SCENARIO C (§7.3): can cycle 1's compiled dependency-module ModGuts be
-- MEMOIZED and reused for cycles 2..N, instead of recompiling the whole
-- stdlib closure every cycle?
--
-- 'externalizeInternalTops' below is LIFTED VERBATIM from GhcPipeline.hs
-- (un-exported there, comments trimmed) — see that module for the #313
-- rationale. Load-bearing for this scenario: 'runCompile' applies it to
-- EVERY module's guts (dep and target alike) before merging
-- (@allBinds = concatMap mg_binds depGuts ++ mg_binds targetGuts@ in
-- GhcPipeline.hs, where @depGuts@/@targetGuts@ are already-externalized), so
-- a faithful memo must store the SAME post-externalize binds a fresh compile
-- would merge — memoizing pre-externalize 'ModGuts' would silently test a
-- different (and production-incorrect) input.
--------------------------------------------------------------------------------

externalizeInternalTops :: ModGuts -> ModGuts
externalizeInternalTops guts = guts { mg_binds = map goTop (mg_binds guts) }
  where
    m = mg_module guts
    topBinders = concatMap binders (mg_binds guts)
      where binders (NonRec b _) = [b]
            binders (Rec ps)     = map fst ps
    fixes = mkVarEnv [ (v, externalize v)
                     | v <- topBinders
                     , not (isExternalName (idName v)) ]
    externalize v =
      let n    = idName v
          u    = nameUnique n
          occ  = nameOccName n
          occ' = mkOccName (occNameSpace occ)
                           (occNameString occ ++ "_u" ++ show (getKey u))
      in setVarName v (mkExternalName u m occ' (nameSrcSpan n))
    sub v = fromMaybe v (lookupVarEnv fixes v)
    goTop (NonRec b rhs) = NonRec (sub b) (goExpr rhs)
    goTop (Rec ps)       = Rec [ (sub b, goExpr rhs) | (b, rhs) <- ps ]
    goBind (NonRec b rhs) = NonRec b (goExpr rhs)
    goBind (Rec ps)       = Rec [ (b, goExpr rhs) | (b, rhs) <- ps ]
    goExpr e = case e of
      Var v            -> Var (sub v)
      Lit _            -> e
      App f a          -> App (goExpr f) (goExpr a)
      Lam b body       -> Lam b (goExpr body)
      Let b body       -> Let (goBind b) (goExpr body)
      Case s b t alts  -> Case (goExpr s) b t
                            [ Alt c bs (goExpr rhs) | Alt c bs rhs <- alts ]
      Cast e' co       -> Cast (goExpr e') co
      Tick t e'        -> Tick t (goExpr e')
      Type _           -> e
      Coercion _       -> e

-- | One module's front-half+back-half+deferred-HPT-registration+externalize,
-- collapsed to exactly the pieces scenario C needs: the externalized binds
-- (what a real merge would use) and the captured @__result@ type (only
-- meaningful for the target). Lifted from the same compileFront/compileBack
-- pair 'runCycle' above uses, minus the recomp/timing bookkeeping that lives
-- at the call site here instead.
compileOneModule :: Set.Set ModuleName -> ModSummary -> Ghc (ModuleName, [CoreBind], Maybe Type)
compileOneModule deferredMods modSum0 = do
  let modSum = modSum0 { ms_hspp_opts = canonicalizeDFlags (ms_hspp_opts modSum0) }
  parsed      <- parseModule modSum
  typechecked <- typecheckModule parsed
  hscEnv0 <- getSession
  let hscEnv   = hscUpdateFlags canonicalizeDFlags hscEnv0
      tcGblEnv = fst (tm_internals_ typechecked)
      mResTy   = capturedBindingType resultBinder tcGblEnv
  desugared  <- liftIO (hscDesugar hscEnv modSum tcGblEnv)
  simplified <- liftIO (core2core hscEnv desugared)
  when (ms_mod_name modSum `Set.member` deferredMods) $ do
    (cgGuts, modDetails) <- liftIO (hscTidy hscEnv simplified)
    iface <- liftIO $
      mkIfaceTc hscEnv Sf_None modDetails modSum (Just (cg_binds cgGuts)) tcGblEnv
    let hmi = HomeModInfo iface modDetails emptyHomeModInfoLinkable
    hscEnvNow <- getSession
    setSession (hscUpdateHPT (\hpt -> addToHpt hpt (ms_mod_name modSum) hmi) hscEnvNow)
  let extern = externalizeInternalTops simplified
  pure (ms_mod_name modSum, mg_binds extern, mResTy)

fmtUnresolved :: UnresolvedVar -> String
fmtUnresolved uv = uvModule uv ++ "." ++ uvName uv

data GutsMemoCycleReport = GutsMemoCycleReport
  { gmCycle           :: Int
  , gmTargetModule    :: String
  , gmRecompiles      :: [(String, String)]
  , gmLoadMs          :: Integer
  , gmFreshLoopMs     :: Integer
    -- ^ Wall-clock of compiling deps+target together (the scenario-B shape,
    -- run again here for a same-run, apples-to-apples comparison).
  , gmMemoLoopMs      :: Integer
    -- ^ Wall-clock of compiling ONLY the target — the scenario-C shape.
  , gmFreshUnresolved :: [String]
  , gmMemoUnresolved  :: [String]
  , gmFreshPoisoned   :: [(Word64, Text)]
  , gmMemoPoisoned    :: [(Word64, Text)]
  , gmFreshNodeCount  :: Int
  , gmMemoNodeCount   :: Int
  , gmUsedMemo        :: Bool
    -- ^ False on cycle 1 (the memo is SEEDED this cycle, nothing to reuse
    -- yet — fresh and memo merges are trivially identical by construction).
  , gmFailure         :: Maybe String
  }

-- | One §7.3 cycle: load' (ModIfaceCache-threaded, per §7.1) + Val injection
-- exactly as 'runCycle' does, then TWO independent compile passes over the
-- SAME post-load'/post-inject session state —
--
--   PASS 1 (fresh):  compile every summary (12 stdlib deps + target) — the
--                     scenario-B shape, replayed here per-cycle so its cost
--                     is measured under the exact same conditions as pass 2.
--   PASS 2 (memo):   compile ONLY the target summary.
--
-- allBindsFresh = pass 1's dep binds ++ pass 1's target binds (a full fresh
-- merge, byte-for-byte what production's 'runCompile' would build this
-- cycle). allBindsMemo = cycle 1's MEMOIZED dep binds (frozen, never
-- refreshed after cycle 1) ++ pass 2's target binds. Both merges are fed to
-- 'translateModuleClosed' in the SAME cycle, off the SAME 'hscFinal' — the
-- only variable between the two 'ClosedModule' results is which dep binds
-- were used, which is exactly the question §7.3 asks.
runGutsMemoCycle
  :: FilePath -> ModIfaceCache -> Int -> [SessionModule]
  -> Maybe (Map.Map ModuleName [CoreBind])
  -> Ghc (GutsMemoCycleReport, [SessionModule], Map.Map ModuleName [CoreBind])
runGutsMemoCycle wd cache k priorVals mDepMemo = do
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
      load' (Just cache) LoadAllTargets mkUnknownDiagnostic (Just (spikeMessager recompRef))
            (mapMG unpoison depGraph)
    case loadFlag of
      Failed    -> liftIO $ ioError $ userError $
        "spike-batch gutsmemo cycle " ++ show k ++ ": PHASE 1 dependency load failed"
      Succeeded -> pure ()
    do hscMG <- getSession
       setSession hscMG { hsc_mod_graph = modGraphRaw }
    hsc0 <- getSession
    hscInjected <- injectSessionScope (SessionScope wd priorVals) hsc0
    setSession hscInjected
    let summaries =
          [ ms | ModuleNode _ ms <- flattenSCCs (topSortModuleGraph True modGraphRaw Nothing)
               , ms_hsc_src ms == HsSrcFile ]
        targetSummary = case [ ms | ms <- summaries, ms_mod_name ms == targetModName' ] of
          (ms:_) -> ms
          []     -> error ("spike-batch gutsmemo cycle " ++ show k ++ ": target summary not found")
        depOrder = [ ms_mod_name ms | ms <- summaries, ms_mod_name ms /= targetModName' ]
    -- PASS 1 (fresh): every summary, deps + target.
    (freshResults, freshLoopMs) <- timeSection $ forM summaries (compileOneModule deferredMods)
    -- PASS 2 (memo): target only.
    (memoResults, memoLoopMs) <- timeSection $ (:[]) <$> compileOneModule deferredMods targetSummary
    let freshByMod = Map.fromList [ (mn, bs) | (mn, bs, _) <- freshResults ]
        (_, memoTargetBinds, _memoTargetTy) = head memoResults
        freshTargetBinds = Map.findWithDefault [] targetModName' freshByMod
        freshTargetTy = case [ ty | (mn, _, ty) <- freshResults, mn == targetModName' ] of
          (t:_) -> t
          []    -> Nothing
        freshDepByMod = Map.delete targetModName' freshByMod
        depMemoToUse  = fromMaybe freshDepByMod mDepMemo
        freshDepBinds = concat [ Map.findWithDefault [] mn freshDepByMod | mn <- depOrder ]
        memoDepBinds  = concat [ Map.findWithDefault [] mn depMemoToUse  | mn <- depOrder ]
        allBindsFresh = freshDepBinds ++ freshTargetBinds
        allBindsMemo  = memoDepBinds  ++ memoTargetBinds
    hscFinal <- getSession
    closedFresh <- liftIO (translateModuleClosed hscFinal allBindsFresh resultBinder)
    closedMemo  <- liftIO (translateModuleClosed hscFinal allBindsMemo  resultBinder)
    resultTy <- case freshTargetTy of
      Just ty -> pure ty
      Nothing -> liftIO $ ioError $ userError $
        "spike-batch gutsmemo cycle " ++ show k ++ ": no " ++ resultBinder ++ " binder captured"
    let bound = stripMonadHead resultTy
        sm    = SessionModule ValMod (Generation (fromIntegral k))
    iface <- liftIO (mkThinSessionIface hscFinal sm [(mkVarOcc "v", bound)])
    liftIO (writeSessionIface hscFinal wd sm iface)
    recomps <- liftIO (reverse <$> readIORef recompRef)
    let report = GutsMemoCycleReport
          { gmCycle           = k
          , gmTargetModule    = modName
          , gmRecompiles      = recomps
          , gmLoadMs          = loadMs
          , gmFreshLoopMs     = freshLoopMs
          , gmMemoLoopMs      = memoLoopMs
          , gmFreshUnresolved = map fmtUnresolved (cmUnresolved closedFresh)
          , gmMemoUnresolved  = map fmtUnresolved (cmUnresolved closedMemo)
          , gmFreshPoisoned   = cmPoisoned closedFresh
          , gmMemoPoisoned    = cmPoisoned closedMemo
          , gmFreshNodeCount  = Seq.length (cmNodes closedFresh)
          , gmMemoNodeCount   = Seq.length (cmNodes closedMemo)
          , gmUsedMemo        = case mDepMemo of { Just _ -> True; Nothing -> False }
          , gmFailure         = Nothing
          }
        outgoingMemo = case mDepMemo of
          Just m  -> m               -- frozen from cycle 1 onward — never refreshed
          Nothing -> freshDepByMod   -- seed the memo from cycle 1's own fresh compile
    pure (report, outgoingMemo)
  case attempt of
    Left (e :: SomeException) ->
      pure ( GutsMemoCycleReport k modName [] 0 0 0 [] [] [] [] 0 0 False (Just (show e))
           , priorVals
           , fromMaybe Map.empty mDepMemo )
    Right (report, outgoingMemo) ->
      pure ( report
           , priorVals ++ [SessionModule ValMod (Generation (fromIntegral k))]
           , outgoingMemo )

-- | Drive cycles 1..3 in order, threading the dep-guts memo forward and
-- stopping (but still reporting) at the first failure.
goGutsMemoCycles
  :: FilePath -> ModIfaceCache -> [Int] -> [SessionModule]
  -> Maybe (Map.Map ModuleName [CoreBind]) -> Ghc [GutsMemoCycleReport]
goGutsMemoCycles _ _ [] _ _ = pure []
goGutsMemoCycles wd cache (k:ks) priorVals mDepMemo = do
  (report, priorVals', outgoingMemo) <- runGutsMemoCycle wd cache k priorVals mDepMemo
  case gmFailure report of
    Just _  -> pure [report]
    Nothing -> (report :) <$> goGutsMemoCycles wd cache ks priorVals' (Just outgoingMemo)

setEq :: Ord a => [a] -> [a] -> Bool
setEq a b = Set.fromList a == Set.fromList b

-- | Print scenario C's per-cycle table and return whether it went GREEN:
-- for every cycle that actually USED the memo (cycle >= 2), the fresh and
-- memoized merges must agree exactly on cmUnresolved and cmPoisoned (as
-- sets — see the doc's own instrument spec: a byte-diff is too strict,
-- unresolved/poisoned are the right comparison).
printGutsMemoReport :: [GutsMemoCycleReport] -> IO Bool
printGutsMemoReport reports = do
  putStrLn "\n################ SCENARIO: C: ModIfaceCache + cycle-1 dep-guts memo (§7.3) ################"
  forM_ reports $ \r -> do
    printf "\n--- cycle %d  target=%s  memoReused=%s ---\n"
      (gmCycle r) (gmTargetModule r) (show (gmUsedMemo r))
    case gmFailure r of
      Just e -> putStrLn ("  FAILED: " ++ unwords (words e))
      Nothing -> do
        let n = length (gmRecompiles r)
            recompiled = length (filter (isRecompiled . snd) (gmRecompiles r))
        printf "  load' recompile verdicts: %d/%d NeedsRecompile\n" recompiled n
        printf "  load' wall-clock:              %d ms\n" (gmLoadMs r)
        printf "  fresh (deps+target) loop ms:   %d ms\n" (gmFreshLoopMs r)
        printf "  memo  (target-only)  loop ms:  %d ms\n" (gmMemoLoopMs r)
        printf "  fresh: unresolved=%s poisoned=%s nodes=%d\n"
          (show (gmFreshUnresolved r)) (show (gmFreshPoisoned r)) (gmFreshNodeCount r)
        printf "  memo:  unresolved=%s poisoned=%s nodes=%d\n"
          (show (gmMemoUnresolved r)) (show (gmMemoPoisoned r)) (gmMemoNodeCount r)
        when (gmFreshNodeCount r /= gmMemoNodeCount r) $
          printf "  ** NODE COUNT DIVERGES: fresh=%d memo=%d **\n" (gmFreshNodeCount r) (gmMemoNodeCount r)
  putStrLn "\n================ VERDICT ================"
  let checked = [ r | r <- reports, gmUsedMemo r, gmFailure r == Nothing ]
      allSucceeded = length reports == 3 && all ((== Nothing) . gmFailure) reports
      agree r = setEq (gmFreshUnresolved r) (gmMemoUnresolved r)
             && setEq (gmFreshPoisoned r) (gmMemoPoisoned r)
      allAgree = not (null checked) && all agree checked
      go = allSucceeded && allAgree
  forM_ checked $ \r -> printf "  cycle %d: agree=%s\n" (gmCycle r) (show (agree r))
  putStrLn $ if go
    then "  GREEN: for every memo-reusing cycle, the memoized-deps merge and the fresh-deps merge produced IDENTICAL cmUnresolved/cmPoisoned."
    else "  RED: see the per-cycle detail above — the memoized merge diverged from the fresh merge on an unresolved or poisoned external."
  pure go

-- | Run the full 3-cycle chain in one fresh 'runGhc' session, ModIfaceCache
-- threaded throughout (per §7.1's settled GREEN fix), for scenario C's own
-- scratch dir.
runGutsMemoScenario :: FilePath -> IO [GutsMemoCycleReport]
runGutsMemoScenario libdir = do
  let wd = workDir "gutsMemo"
  exists <- doesDirectoryExist wd
  when exists (removeDirectoryRecursive wd)
  createDirectoryIfMissing True wd
  forM_ [1, 2, 3] $ \k -> writeFile (wd </> ("Input" ++ show k ++ ".hs")) (inputSrc k)
  cache <- newIfaceCache
  runGhc (Just libdir) $ do
    dflags <- getSessionDynFlags
    _ <- setSessionDynFlags (extractionDynFlags dflags [wd, "lib"])
    goGutsMemoCycles wd cache [1, 2, 3] [] Nothing

--------------------------------------------------------------------------------
-- SCENARIO D (§7.6's named gap, batch-extract lane): scenario C's memo is
-- FROZEN after cycle 1 — sound only because its dep closure never changes
-- shape across cycles. §7.6 names the real-world case that breaks that
-- assumption: a module introduced PARTWAY through the batch (a decl item's
-- own @Lib.G<g>@, or — the shape probed here, which needs no decl machinery
-- to demonstrate the same mechanism — a later item's target simply importing
-- one more home module than an earlier item's did). The fix is a
-- generalization, not a different mechanism: memoize PER MODULE, populated
-- INCREMENTALLY on that module's FIRST compile in the batch, whichever cycle
-- that is. This scenario measures it directly: cycle 1's target imports only
-- @Tidepool.Prelude@; cycles 2 and 3 ALSO import a new home module, @Extra@,
-- which therefore has NOTHING to memoize at cycle 1 and must be memoized for
-- the FIRST time at cycle 2, then reused (not recompiled) at cycle 3.
--------------------------------------------------------------------------------

-- | @Extra@'s own source: a small, genuine home-package module (real source
-- on disk, mirroring how a decl item's @Lib.G<g>@ is "genuine source the
-- Rust side renders and writes" per the feasibility doc §2.1) that only
-- cycles 2 and 3's targets import — cycle 1's target never references it.
extraModuleSrc :: String
extraModuleSrc = unlines
  [ "{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}"
  , "module Extra where"
  , "import Tidepool.Prelude"
  , ""
  , "helper :: Text -> Text"
  , "helper t = t <> \"_extra\""
  ]

-- | The 3-target chain for scenario D. Unlike 'inputSrc' (scenario A/B/C),
-- this is NOT a Val-injection chain — deliberately, to isolate the ONE
-- variable this scenario probes (a new home module appearing mid-batch) from
-- the (already-settled, §2.1/§7.1) binder-chain question. Cycle 1 imports
-- only 'Tidepool.Prelude'; cycles 2 and 3 ALSO import 'Extra'.
incInputSrc :: Int -> String
incInputSrc 1 = unlines
  [ "{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}"
  , "module IncInput1 where"
  , "import Tidepool.Prelude"
  , ""
  , "__result :: Text"
  , "__result = toUpper \"hello1\""
  ]
incInputSrc k = unlines
  [ "{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}"
  , "module IncInput" ++ show k ++ " where"
  , "import Tidepool.Prelude"
  , "import Extra (helper)"
  , ""
  , "__result :: Text"
  , "__result = helper (toUpper \"hello" ++ show k ++ "\")"
  ]

data IncMemoCycleReport = IncMemoCycleReport
  { imCycle             :: Int
  , imTargetModule      :: String
  , imNewModules        :: [String]
    -- ^ Modules compiled and memoized for the FIRST time THIS cycle
    -- (excluding the target, which is never memoized — see the module doc).
  , imReusedModules     :: [String]
    -- ^ Modules whose binds were REUSED from the memo this cycle (populated
    -- by an EARLIER cycle — possibly, but not necessarily, cycle 1).
  , imLoadMs            :: Integer
  , imFreshLoopMs       :: Integer
    -- ^ Wall-clock of compiling every summary (deps + target) fresh, no
    -- memo — the production-shape cost the incremental memo is compared
    -- against.
  , imIncMemoLoopMs     :: Integer
    -- ^ Wall-clock of the incremental-memo pass: reuse what's memoized,
    -- compile (and memoize) only what's new this cycle, plus the target.
  , imFreshUnresolved   :: [String]
  , imIncMemoUnresolved :: [String]
  , imFreshPoisoned     :: [(Word64, Text)]
  , imIncMemoPoisoned   :: [(Word64, Text)]
  , imFreshNodeCount    :: Int
  , imIncMemoNodeCount  :: Int
  , imFailure           :: Maybe String
  }

-- | One §7.6-gap cycle: load' (ModIfaceCache-threaded) + the same
-- deferred-target dance 'runGutsMemoCycle' uses, then TWO independent
-- compile passes over the SAME post-load'/post-inject session state —
--
--   PASS FRESH:     compile EVERY summary (deps + target), no memo at all —
--                    what production's per-cycle loop does with the memo
--                    disabled, replayed here for an apples-to-apples
--                    same-cycle comparison.
--   PASS INC-MEMO:   for each summary, reuse its binds from @memoSoFar@ if
--                    present (populated by ANY earlier cycle, not just
--                    cycle 1); otherwise compile it fresh and record it as
--                    newly memoized THIS cycle. The target is always
--                    recompiled (never memoized — each item's own turn text
--                    differs by construction).
--
-- Both merges are fed to the SAME, unmodified 'translateModuleClosed', off
-- the SAME 'hscFinal' — the only variable is which dep binds were used.
runIncrementalMemoCycle
  :: FilePath -> ModIfaceCache -> Int -> Map.Map ModuleName [CoreBind]
  -> Ghc (IncMemoCycleReport, Map.Map ModuleName [CoreBind])
runIncrementalMemoCycle wd cache k memoSoFar = do
  let path          = wd </> ("IncInput" ++ show k ++ ".hs")
      modName       = "IncInput" ++ show k
      targetModName' = mkModuleName modName
  attempt <- try $ do
    target <- guessTarget path Nothing Nothing
    setTargets [target]
    modGraphRaw <- depanal [] False
    let deferredMods = Set.singleton targetModName'
        depGraph = mkModuleGraph
          [ node | node <- mgModSummaries' modGraphRaw
                 , case node of
                     ModuleNode _ ms -> not (ms_mod_name ms `Set.member` deferredMods)
                     _               -> True ]
        unpoison ms =
          ms { ms_hspp_opts = gopt_unset (ms_hspp_opts ms) Opt_IgnoreInterfacePragmas }
    recompRef <- liftIO (newIORef [])
    (loadFlag, loadMs) <- timeSection $
      load' (Just cache) LoadAllTargets mkUnknownDiagnostic (Just (spikeMessager recompRef))
            (mapMG unpoison depGraph)
    case loadFlag of
      Failed    -> liftIO $ ioError $ userError $
        "spike-batch incmemo cycle " ++ show k ++ ": PHASE 1 dependency load failed"
      Succeeded -> pure ()
    do hscMG <- getSession
       setSession hscMG { hsc_mod_graph = modGraphRaw }
    let summaries =
          [ ms | ModuleNode _ ms <- flattenSCCs (topSortModuleGraph True modGraphRaw Nothing)
               , ms_hsc_src ms == HsSrcFile ]
        depOrder = [ ms_mod_name ms | ms <- summaries, ms_mod_name ms /= targetModName' ]
    -- PASS FRESH: every summary, deps + target, no memo.
    (freshResults, freshLoopMs) <- timeSection $ forM summaries (compileOneModule deferredMods)
    -- PASS INC-MEMO: reuse from 'memoSoFar' where present; compile+memoize
    -- what's new; always recompile the target.
    (incResults, incLoopMs) <- timeSection $ forM summaries $ \ms -> do
      let mn = ms_mod_name ms
      if mn == targetModName'
        then do
          (_, binds, ty) <- compileOneModule deferredMods ms
          pure (mn, binds, ty, True)
        else case Map.lookup mn memoSoFar of
          Just binds -> pure (mn, binds, Nothing, False)
          Nothing    -> do
            (_, binds, ty) <- compileOneModule deferredMods ms
            pure (mn, binds, ty, True)
    let newModNames    = [ moduleNameString mn | (mn, _, _, isNew) <- incResults, isNew, mn /= targetModName' ]
        reusedModNames = [ moduleNameString mn | (mn, _, _, isNew) <- incResults, not isNew ]
        updatedMemo = foldl' (\m (mn, binds, _, isNew) ->
                                if mn /= targetModName' && isNew then Map.insert mn binds m else m)
                              memoSoFar incResults
        incByMod    = Map.fromList [ (mn, binds) | (mn, binds, _, _) <- incResults ]
        freshByMod  = Map.fromList [ (mn, binds) | (mn, binds, _) <- freshResults ]
        freshTargetTy = case [ ty | (mn, _, ty) <- freshResults, mn == targetModName' ] of
          (Just t : _) -> Just t
          _            -> Nothing
        freshTargetBinds = Map.findWithDefault [] targetModName' freshByMod
        incTargetBinds   = Map.findWithDefault [] targetModName' incByMod
        allBindsFresh = concat [ Map.findWithDefault [] mn freshByMod | mn <- depOrder ] ++ freshTargetBinds
        allBindsInc   = concat [ Map.findWithDefault [] mn incByMod   | mn <- depOrder ] ++ incTargetBinds
    hscFinal <- getSession
    closedFresh <- liftIO (translateModuleClosed hscFinal allBindsFresh resultBinder)
    closedInc   <- liftIO (translateModuleClosed hscFinal allBindsInc   resultBinder)
    case freshTargetTy of
      Just _  -> pure ()
      Nothing -> liftIO $ ioError $ userError $
        "spike-batch incmemo cycle " ++ show k ++ ": no " ++ resultBinder ++ " binder captured"
    let report = IncMemoCycleReport
          { imCycle = k, imTargetModule = modName
          , imNewModules = newModNames, imReusedModules = reusedModNames
          , imLoadMs = loadMs, imFreshLoopMs = freshLoopMs, imIncMemoLoopMs = incLoopMs
          , imFreshUnresolved   = map fmtUnresolved (cmUnresolved closedFresh)
          , imIncMemoUnresolved = map fmtUnresolved (cmUnresolved closedInc)
          , imFreshPoisoned     = cmPoisoned closedFresh
          , imIncMemoPoisoned   = cmPoisoned closedInc
          , imFreshNodeCount    = Seq.length (cmNodes closedFresh)
          , imIncMemoNodeCount  = Seq.length (cmNodes closedInc)
          , imFailure = Nothing
          }
    pure (report, updatedMemo)
  case attempt of
    Left (e :: SomeException) ->
      pure ( IncMemoCycleReport k modName [] [] 0 0 0 [] [] [] [] 0 0 (Just (show e))
           , memoSoFar )
    Right r -> pure r

goIncMemoCycles :: FilePath -> ModIfaceCache -> [Int] -> Map.Map ModuleName [CoreBind] -> Ghc [IncMemoCycleReport]
goIncMemoCycles _ _ [] _ = pure []
goIncMemoCycles wd cache (k : ks) memoSoFar = do
  (report, updatedMemo) <- runIncrementalMemoCycle wd cache k memoSoFar
  case imFailure report of
    Just _  -> pure [report]
    Nothing -> (report :) <$> goIncMemoCycles wd cache ks updatedMemo

-- | Print scenario D's per-cycle table and return whether it went GREEN:
-- for every cycle, the fresh and incremental-memo merges must agree exactly
-- on cmUnresolved/cmPoisoned — INCLUDING cycle 2, where @Extra@ has nothing
-- to reuse yet and must be memoized fresh for the first time, and cycle 3,
-- where @Extra@ IS reused from the memo cycle 2 populated (not cycle 1).
printIncMemoReport :: [IncMemoCycleReport] -> IO Bool
printIncMemoReport reports = do
  putStrLn "\n################ SCENARIO: D: incremental per-module memo, new module mid-batch (§7.6 gap) ################"
  forM_ reports $ \r -> do
    printf "\n--- cycle %d  target=%s ---\n" (imCycle r) (imTargetModule r)
    case imFailure r of
      Just e -> putStrLn ("  FAILED: " ++ unwords (words e))
      Nothing -> do
        printf "  new-this-cycle (memoized now): %s\n" (show (imNewModules r))
        printf "  reused-from-memo:              %s\n" (show (imReusedModules r))
        printf "  load' wall-clock:              %d ms\n" (imLoadMs r)
        printf "  fresh (deps+target) loop ms:   %d ms\n" (imFreshLoopMs r)
        printf "  inc-memo loop ms:              %d ms\n" (imIncMemoLoopMs r)
        printf "  fresh:   unresolved=%s poisoned=%s nodes=%d\n"
          (show (imFreshUnresolved r)) (show (imFreshPoisoned r)) (imFreshNodeCount r)
        printf "  inc-memo: unresolved=%s poisoned=%s nodes=%d\n"
          (show (imIncMemoUnresolved r)) (show (imIncMemoPoisoned r)) (imIncMemoNodeCount r)
        when (imFreshNodeCount r /= imIncMemoNodeCount r) $
          printf "  ** NODE COUNT DIVERGES: fresh=%d inc-memo=%d **\n" (imFreshNodeCount r) (imIncMemoNodeCount r)
  putStrLn "\n================ VERDICT ================"
  let allSucceeded = length reports == 3 && all ((== Nothing) . imFailure) reports
      agree r = setEq (imFreshUnresolved r) (imIncMemoUnresolved r)
             && setEq (imFreshPoisoned r) (imIncMemoPoisoned r)
      allAgree = allSucceeded && all agree reports
      go = allSucceeded && allAgree
  forM_ reports $ \r -> when (imFailure r == Nothing) $ printf "  cycle %d: agree=%s\n" (imCycle r) (show (agree r))
  putStrLn $ if go
    then "  GREEN: the incremental (populated-on-first-sight, never frozen) per-module memo agrees with a fresh recompile on cmUnresolved/cmPoisoned, on every cycle -- INCLUDING the cycle a module first appears (cycle 2) and the cycle that reuses it from a memo entry NOT seeded at cycle 1 (cycle 3)."
    else "  RED: see the per-cycle detail above -- the incremental memo diverged from a fresh recompile on an unresolved or poisoned external."
  pure go

-- | Run the 3-cycle chain in one fresh 'runGhc' session, ModIfaceCache
-- threaded throughout, for scenario D's own scratch dir (+ 'Extra.hs',
-- written once, present for every cycle regardless of whether that cycle's
-- own target imports it).
runIncrementalMemoScenario :: FilePath -> IO [IncMemoCycleReport]
runIncrementalMemoScenario libdir = do
  let wd = workDir "incMemo"
  exists <- doesDirectoryExist wd
  when exists (removeDirectoryRecursive wd)
  createDirectoryIfMissing True wd
  writeFile (wd </> "Extra.hs") extraModuleSrc
  forM_ [1, 2, 3] $ \k -> writeFile (wd </> ("IncInput" ++ show k ++ ".hs")) (incInputSrc k)
  cache <- newIfaceCache
  runGhc (Just libdir) $ do
    dflags <- getSessionDynFlags
    _ <- setSessionDynFlags (extractionDynFlags dflags [wd, "lib"])
    goIncMemoCycles wd cache [1, 2, 3] Map.empty

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
--
-- A third scenario, C, answers §7.3 (a SEPARATE runGhc session, its own
-- 'newIfaceCache'): does memoizing cycle 1's compiled dependency-module
-- 'ModGuts' let cycles 2..N skip recompiling them entirely, merging the
-- memo with just the target's own compile — and does that memoized merge
-- resolve identically (same cmUnresolved/cmPoisoned) to a fresh-deps merge
-- computed the SAME cycle? See 'runGutsMemoCycle''s haddock.
--
-- A fourth scenario, D (batch-extract lane, §7.6's named-but-unmeasured
-- gap): does the GENERALIZATION — memoize per module, populated
-- INCREMENTALLY on that module's first compile in the batch, rather than
-- frozen after cycle 1 — still agree with a fresh recompile once a module
-- appears for the first time PARTWAY through the batch (cycle 2, not cycle
-- 1) and is then reused from a memo entry cycle 1 never seeded? See
-- 'runIncrementalMemoCycle''s haddock.
--
-- Exit code: NOT gated on any scenario's GREEN/RED verdict — A's own RED
-- (production's real `load' Nothing ...` behaviour) is the documented,
-- correct answer to §2.3 as posed, and forcing the process to fail because
-- of a truthfully-reported RED would be exactly the "force a green"
-- anti-pattern this probe (and the one before it) is told not to commit.
-- The exit code instead reports whether all four scenarios RAN CLEANLY —
-- every cycle completed without an internal exception — which is the
-- correct pass/fail contract for an instrument whose job is to gather
-- trustworthy evidence, not to pre-judge what that evidence says.
main :: IO ()
main = do
  libdir <- getLibdir
  reportsA <- runScenario libdir "noCache" Nothing
  _goA <- printReport "A: no ModIfaceCache (matches production `load' Nothing ...`)" reportsA
  cache <- newIfaceCache
  reportsB <- runScenario libdir "withCache" (Just cache)
  _goB <- printReport "B: WITH a live ModIfaceCache threaded across all 3 cycles" reportsB
  reportsC <- runGutsMemoScenario libdir
  _goC <- printGutsMemoReport reportsC
  reportsD <- runIncrementalMemoScenario libdir
  _goD <- printIncMemoReport reportsD
  let cleanRun rs failureOf = length rs == 3 && all ((== Nothing) . failureOf) rs
      ranCleanly = cleanRun reportsA crFailure
                && cleanRun reportsB crFailure
                && cleanRun reportsC gmFailure
                && cleanRun reportsD imFailure
  if ranCleanly then exitSuccess else exitFailure
