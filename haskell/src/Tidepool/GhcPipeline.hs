module Tidepool.GhcPipeline
  ( runPipeline, runPipelineSession, PipelineResult(..), dumpCore
    -- * Bound-value type analysis (Wave 3b BIND mode)
  , stripMonadHead, isClosureType, renderType
  , splitTupleType ) where

import GHC
import GHC.Driver.Main (hscDesugar, batchMsg, hscTidy)
import GHC.Driver.Env (hscUpdateFlags, hscUpdateHPT)
import GHC.Driver.Env.Types (HscEnv(hsc_mod_graph))
import GHC.Unit.Home (homeUnitId)
import GHC.Unit.Home.ModInfo (HomeModInfo(..), emptyHomeModInfoLinkable, addToHpt)
import GHC.Driver.Make (load')
import GHC.Iface.Make (mkIfaceTc)
import GHC.Types.SafeHaskell (SafeHaskellMode(Sf_None))
import GHC.Types.SourceFile (HscSource(..))
import GHC.Types.Error (mkUnknownDiagnostic, MessageClass(..), Severity(..), mkLocMessage)
import GHC.Types.SrcLoc (unLoc)
import GHC.Utils.Logger (LogAction)
import GHC.Data.FastString (unpackFS)
import GHC.Unit.Module.Graph (mapMG, mkModuleGraph, mgModSummaries', ModuleGraphNode(..))
import GHC.Data.Graph.Directed (flattenSCCs)
import GHC.Core.Opt.Pipeline (core2core)
import GHC.Core.Ppr (pprCoreBindings)
import GHC.Driver.Session
  ( updOptLevel, gopt_set, gopt_unset
  , packageFlags, PackageFlag(..), PackageArg(..), ModRenaming(..) )
import GHC.Unit.Module.ModGuts (ModGuts(..), CgGuts(..))
import GHC.Core (CoreBind, CoreExpr, Bind(..), Expr(..), Alt(..))
import qualified Data.Set as Set
import qualified Data.Map.Strict as Map
import GHC.Platform (genericPlatform)
import GHC.Utils.Outputable (renderWithContext, defaultSDocContext, ppr)
import GHC.Types.Id (idName, idType)
import GHC.Core.Type (Type, splitAppTy_maybe, splitTyConApp_maybe, isFunTy)
import GHC.Core.TyCon (isTupleTyCon)
import GHC.Tc.Utils.TcType (tcSplitSigmaTy)
import GHC.Types.TypeEnv (typeEnvIds)
import GHC.Tc.Types (TcGblEnv, tcg_type_env)
import GHC.Types.Name (nameOccName, nameUnique, mkExternalName, nameModule_maybe)
import GHC.Types.Name.Occurrence (mkOccName, occNameSpace, occNameString)
import GHC.Types.Var (setVarName)
import GHC.Types.Var.Env (mkVarEnv, lookupVarEnv)
import GHC.Types.Unique (getKey)
import Control.Applicative ((<|>))
import Data.Maybe (fromMaybe)
import Data.List (nub)
import Data.IORef (IORef, newIORef, modifyIORef', readIORef)
import System.Process (readProcess)
import System.Environment (lookupEnv)
import System.FilePath (takeBaseName)
import System.IO (hPutStrLn, stderr)
import Control.Monad.IO.Class (liftIO)
import Control.Monad (forM, when)
import Data.Char (toUpper)
import Tidepool.Session
  ( SessionScope(..), isSessionScopeActive, injectSessionScope, renderSessionModule )
import Tidepool.Timing (readTimingEnabled, timeSection, emitPhase, monotonicTime, elapsedMs)

data PipelineResult = PipelineResult
  { prBinds  :: [CoreBind]
  , prTyCons :: [TyCon]
  , prHscEnv :: HscEnv
  -- | The GHC-inferred type of the target module's @__user@ binding (the eval's
  -- top-level expression), rendered to a string via 'ppr'. 'Nothing' when no
  -- @__user@ binding is present (e.g. fixture/Suite extraction). Captured at the
  -- typecheck stage because our CBOR serializer strips all type information.
  --
  -- CAVEAT (Wave-4): 'ppr' rendering is NOT parser-faithful — it can elide
  -- qualifiers / use unicode that won't round-trip through GHC's parser. Fine
  -- for v1 display + the synthetic @x :: <type>@ decl when the type is simple,
  -- but cross-turn typechecking of references may need a structured
  -- @IfaceType@ instead of this string.
  , prCapturedType :: Maybe String
  -- | The GHC 'Type' of the target module's @result@ binding, captured for the
  -- Wave-3b BIND mode (the value-binding turn). For @result = do { x <- action;
  -- pure x } :: Eff stack T@ this is the FULL @Eff stack T@; 'stripMonadHead'
  -- recovers the bound value type @T@. 'Nothing' when the module has no @result@
  -- binder (every non-bind extraction — reference turns, fixtures, one-shot
  -- evals — so the field is inert off the bind path).
  , prResultType :: Maybe Type
  -- | GHC diagnostic warnings (@-Wincomplete-patterns@, name shadowing, ...)
  -- emitted while compiling the TARGET module — dependency modules (the
  -- preamble, stdlib) are excluded, see 'warnCollectorHook'. Rendered by
  -- GHC's own diagnostic pretty-printer, so a warning carries its
  -- @Expr.hs:<line>:<col>@ location exactly like a compile error does. Empty
  -- on a clean compile.
  , prWarnings :: [String]
  }

-- | The normal one-shot eval extraction. Byte-identical to its historical
-- behaviour: it is exactly @runPipelineSession Nothing@, so no session
-- machinery (iface injection, source-less home modules) ever touches this path.
runPipeline :: FilePath -> [FilePath] -> IO PipelineResult
runPipeline = runPipelineSession Nothing

-- | Extraction with optional tidepool-repl SESSION scope (Option-C type plane).
--
-- @Nothing@ (or an inert 'SessionScope') → the ordinary @depanal@/@load@
-- downsweep path, unchanged. @Just@ an ACTIVE scope → inject the live session
-- @Val.G<g>@ ifaces into the HPT, then compile EVERY home module (deps + target)
-- to optimized Core exactly like the normal path — the only differences from
-- 'runNormalPipeline' are excluding the source-less @Val.G<g>@ modules from the
-- downsweep and injecting their ifaces before compilation
-- (plans/ghci-implementation-plan.md §2 step 4 / §5.3 "C GATE").
--
-- The gate is the @case@ below: the session arm runs ONLY for an active scope.
runPipelineSession :: Maybe SessionScope -> FilePath -> [FilePath] -> IO PipelineResult
runPipelineSession mscope path includes
  | Just scope <- mscope, isSessionScopeActive scope =
      runSessionPipeline scope path includes
  | otherwise = runNormalPipeline path includes

runNormalPipeline :: FilePath -> [FilePath] -> IO PipelineResult
runNormalPipeline path includes = do
  timing <- readTimingEnabled
  (libdir, startupMs) <- timeSection getLibdir
  emitPhase timing "startup" startupMs
  runGhc (Just libdir) $ do
    sessionT0 <- monotonicTime
    dflags <- getSessionDynFlags
    -- Force x86_64-linux target platform regardless of host architecture.
    -- The Cranelift JIT has a single backend; we need deterministic Core IR
    -- with x86_64 primops on all hosts (including ARM/macOS).
    -- Use genericPlatform verbatim — mixing in host platform_constants causes
    -- GHC's specializer to produce Core with mismatched constructor tags on
    -- aarch64, leading to case-exhaustion SIGILL in the JIT.
    -- Platform spoofing happens HERE ONLY (session setup, before 'load'):
    -- GHC populates platform constants during session/unit initialization,
    -- so re-pinning bare genericPlatform later strips them
    -- ("Platform constants not available!" panic). Backend/opt pinning lives
    -- in canonicalizeDFlags and is also re-applied per-module below.
    -- Expose the (otherwise hidden) `ghc` package to the session so lib
    -- modules on the --include path can import the GHC API. The [fmt|]
    -- quasi-quoter's hole parser is the vendored Tidepool.QQ.HsMeta.*, which
    -- runs GHC's own expression parser inside the splice; those modules import
    -- GHC.Parser.* / GHC.Types.* etc. Without this, compiling Tidepool.QQ
    -- fails with "member of the hidden package ghc-9.12.2".
    let dflags' = extractionDynFlags dflags includes
    setSessionDynFlags dflags'
    target <- guessTarget path Nothing Nothing
    setTargets [target]
    -- Success-path warning capture (see 'warnCollectorHook'): installed before
    -- any typecheck runs so every diagnostic the per-module loop below emits
    -- for the target file is recorded, not just printed.
    warnRef <- liftIO (newIORef [])
    pushLogHookM (warnCollectorHook path warnRef)
    -- EPS unpoisoning (QQ/TH support — see canonicalizeDFlags haddock).
    -- 'depanal' runs downsweep, whose @enableCodeGenForTH@ downgrades the
    -- splice-needed home modules' ms_hspp_opts to -O0 +
    -- Opt_IgnoreInterfacePragmas so 'load' can provision bytecode. That flag
    -- ALSO governs how external interfaces are READ, and the downgraded
    -- modules compile FIRST, so the session-global External Package State
    -- would cache every interface they demand (GHC.Num, GHC.Float,
    -- freer-simple, …) WITHOUT unfoldings — the -O2 extraction loop below
    -- then can never fire class-op rules (@negate $fNumDouble@ never
    -- reduces, chasing Integer machinery → "Unsupported primop: clz#").
    -- Unset JUST that flag on every summary BEFORE compilation: the
    -- backend/-O0 downgrade stays (splices still provision via bytecode),
    -- but interface loading honors pragmas, so the EPS is healthy from the
    -- start. Non-TH graphs carry no downgrade — the unset is a no-op there.
    --
    -- A post-'load' EPS flush (the previous fix) does NOT work: home-module
    -- TyCons are already realized in the HPT, so re-typechecking lib modules
    -- never re-demands the package interfaces that define their instances —
    -- they never re-enter the fresh EPS, and typechecking fails with e.g.
    -- "No instance for Monad (Eff '[Console, …])".
    modGraphRaw <- depanal [] False
    -- 'ghc_setup' phase (TIDEPOOL_TIMING): session DynFlags setup +
    -- guessTarget/setTargets + this 'depanal' call, nothing else. FLAT and
    -- non-overlapping with 'ghc_load' below — see Tidepool.Timing's module
    -- haddock and the 'PHASE_GHC_SESSION' tombstone in timing.rs: this pair
    -- retired the old 'ghc_session' bracket on the compile lane (a
    -- collector recovers the historical figure as 'ghc_setup' + 'ghc_load').
    setupT1 <- monotonicTime
    liftIO (emitPhase timing "ghc_setup" (elapsedMs sessionT0 setupT1))
    -- unpoison: keep the EPS healthy under the TH/QQ downgrade by unsetting
    -- Opt_IgnoreInterfacePragmas on every summary (see the depanal/load'
    -- haddock above). The bytecode-vs-object provisioning choice is made
    -- session-wide in canonicalizeDFlags (Opt_UseBytecodeRatherThanObjects) —
    -- it has to be set before downsweep, since 'load' re-derives each module's
    -- backend and ignores a field patched onto a summary here.
    let unpoison ms =
          ms { ms_hspp_opts = gopt_unset (ms_hspp_opts ms) Opt_IgnoreInterfacePragmas }
    loadT0 <- monotonicTime
    loadFlag <- load' Nothing LoadAllTargets mkUnknownDiagnostic (Just batchMsg)
               (mapMG unpoison modGraphRaw)
    loadT1 <- monotonicTime
    -- 'ghc_load' phase (TIDEPOOL_TIMING): the 'load'' call alone, nothing
    -- else. FLAT — see 'ghc_setup' above; the two rows partition what
    -- 'ghc_session' used to bracket, they do not nest inside it.
    liftIO (emitPhase timing "ghc_load" (elapsedMs loadT0 loadT1))
    modGraph <- getModuleGraph
    -- hs-boot summaries are EXCLUDED from extraction (item 20, 2026-08-10):
    -- a boot node shares its ModuleName with the real module, so its
    -- near-empty desugared guts would CLOBBER the real module's entry in
    -- the name-keyed 'gutsByMod' below — hiding every Core edge out of that
    -- module from 'reachableModuleClosure' and silently tiering its
    -- dependencies out of PASS 2 (observed live: the Even.hs-boot/Odd cycle
    -- baked a TypeMetadata sentinel for Odd.odd'). Boot files exist for
    -- 'load''s loop-breaking only; any error in one already surfaced there,
    -- and their guts carry no bindings extraction could use.
    let summaries =
          [ ms | ms <- mgModSummaries modGraph, ms_hsc_src ms == HsSrcFile ]
    when (null summaries) $
      liftIO $ ioError (userError "runPipeline: empty module graph")
    -- E6 (tiered -O2): process every module's parse/typecheck/desugar (PASS
    -- 1, below), then run the expensive optimized-Core pipeline ('core2core',
    -- canonicalizeDFlags' -O2 + exposed-unfoldings) only for the target and
    -- its Core-REACHABLE dependencies (PASS 2) — see 'reachableModuleClosure'
    -- for the exact rule and its soundness argument. A module outside that
    -- set still gets parsed/typechecked (its diagnostics still surface) and
    -- desugared (needed to compute reachability itself — see PASS 1's
    -- comment for why desugar, not just typecheck, is the right stage), but
    -- never pays 'core2core'.
    -- 'typecheck'/'core' are summed ACROSS this loop (one line each, emitted
    -- after) rather than timed per-module: the wire grammar is one line per
    -- phase per process, and a turn module always compiles alongside its
    -- preamble/stdlib dep modules in the same loop. 'core' sums EVERY
    -- module's desugar time plus 'core2core' time for reachable modules only
    -- — same phase definition as before (desugar+core2core, summed across
    -- every module), just less of it now executes.
    tcMsRef <- liftIO (newIORef (0 :: Integer))
    coreMsRef <- liftIO (newIORef (0 :: Integer))
    -- Diagnostic-only accounting (NOT part of the 'tidepool-timing phase=…'
    -- wire grammar 'emitPhase' owns — see the line emitted below, distinctly
    -- prefixed 'e6-tier', deliberately outside that contract so
    -- 'ExtractTiming::parse' never has to know about it): the desugar/
    -- core2core split WITHIN the 'core' phase, plus how many modules the tier
    -- actually spared. 'core' stays one flat phase (desugar summed over every
    -- module + core2core summed over reachable ones only) per the timing
    -- contract; this line exists purely so E6's own report can size its win
    -- against what it actually removed (core2core) rather than the whole
    -- 'core' bucket (desugar + core2core), which is a larger, unmeasured-by-
    -- this-item quantity.
    dsMsRef <- liftIO (newIORef (0 :: Integer))
    c2cMsRef <- liftIO (newIORef (0 :: Integer))
    let targetModName = capitalize (takeBaseName path)
        targetModName' = mkModuleName targetModName
    -- PASS 1 — parse/typecheck/desugar EVERY module. Re-canonicalize each
    -- module's DynFlags first (see canonicalizeDFlags): the load phase may
    -- have downgraded them for TH/QQ bytecode provisioning. NOTE:
    -- 'hscDesugar' does not consume the optLevel/unfolding-exposure flags
    -- 'canonicalizeDFlags' sets (those govern 'core2core' alone, in PASS 2
    -- below), so applying it unconditionally here costs nothing extra even
    -- for a module PASS 2 goes on to tier down.
    passOne <- forM summaries $ \modSum0 -> do
      let modSum = modSum0 { ms_hspp_opts = canonicalizeDFlags (ms_hspp_opts modSum0) }
      (typechecked, tcMs) <- timeSection $ do
        parsed <- parseModule modSum
        typecheckModule parsed
      liftIO (modifyIORef' tcMsRef (+ tcMs))
      hscEnv0 <- getSession
      let hscEnv = hscUpdateFlags canonicalizeDFlags hscEnv0
      let tcGblEnv = fst (tm_internals_ typechecked)
      -- Capture the inferred type of the eval's top expression NOW, before
      -- optimization can inline/rename @__user@ away. Types live on the Id in
      -- the typechecked type env; our CBOR drops them downstream (Translate.hs).
      let mCapturedTy = capturedUserType tcGblEnv
          -- This path is shared by 'runPipeline' (single-shot eval, whose
          -- wrapper always compiles a target named @result@) AND a repl
          -- session's FIRST turn (no prior bindings yet to inject, so
          -- 'isSessionScopeActive' is still False and 'runSessionPipeline'
          -- below is never reached — its wrapper compiles @__result@, the
          -- scaffold-reserved name; see 'processSessionFile'). Try both.
          mResultTy   = capturedBindingType "result" tcGblEnv
                          <|> capturedBindingType "__result" tcGblEnv
      (desugared, dsMs) <- timeSection $ liftIO (hscDesugar hscEnv modSum tcGblEnv)
      liftIO (modifyIORef' coreMsRef (+ dsMs))
      liftIO (modifyIORef' dsMsRef (+ dsMs))
      pure (ms_mod_name modSum, hscEnv, desugared, mCapturedTy, mResultTy)
    -- Reachable-module rule (written down before implementation, per spec):
    -- a home module is REACHABLE from the target iff it IS the target, or its
    -- DESUGARED Core is transitively referenced — via a real 'Var' occurrence
    -- at any depth — from the target module's own desugared Core. Computed on
    -- desugared (pre-'core2core') Core specifically: by desugar time,
    -- typeclass/instance selection is already resolved to explicit
    -- dictionary-Var applications, so this walk would NOT miss a module
    -- imported only for an orphan instance the way a renamer/typecheck-level
    -- "used name" scan would — exactly the silent ErrorSentinel-poisoning
    -- shape 'runSessionPipeline's PHASE 3 comment documents. And because this
    -- codebase defines no @{-# RULES #-}@ anywhere (grep-confirmed empty),
    -- 'core2core' cannot introduce a genuinely NEW cross-module reference that
    -- wasn't already visible at this desugared stage — dictionary/instance
    -- selection is the only mechanism that could hide a reference pre-Core,
    -- and it's already resolved by here. So the desugar-stage reference graph
    -- is a sound (superset-or-equal) approximation of what the fully
    -- optimized Core actually needs.
    --
    -- A module OUTSIDE this closure would contribute zero bindings the
    -- target's Core can reach either way — Main.hs's own post-hoc,
    -- binding-level reachability walk over the final merged 'allBinds'
    -- ('Translate.reachableBinds', run from the target) would discard it
    -- regardless — so skipping 'core2core' for it changes no wire byte on a
    -- passing extraction; it only skips work whose result was always going to
    -- be thrown away.
    forceValidationOnly <- liftIO (lookupEnv "TIDEPOOL_TEST_FORCE_VALIDATION_ONLY")
    let gutsByMod = Map.fromList [ (m, g) | (m, _, g, _, _) <- passOne ]
        reachableMods0 = reachableModuleClosure targetModName' gutsByMod
        -- E6 mis-tiering fault injection (detection-power demonstration,
        -- see 00-spec.md's VERIFY section): forcibly deny a NAMED module
        -- 'core2core' regardless of whether the real closure above found it
        -- reachable — simulating exactly the mistake this tier could make.
        -- Inert unless set; never set outside a deliberate test.
        reachableMods = case forceValidationOnly of
          Just m  -> Set.delete (mkModuleName m) reachableMods0
          Nothing -> reachableMods0
    -- PASS 2 — core2core (canonicalizeDFlags' -O2 + exposed unfoldings) only
    -- for modules in 'reachableMods'.
    results <- fmap concat $ forM passOne $ \(modName, hscEnv, desugared, mCapturedTy, mResultTy) ->
      if modName `Set.member` reachableMods
        then do
          (simplified, coreMs) <- timeSection $ liftIO (core2core hscEnv desugared)
          liftIO (modifyIORef' coreMsRef (+ coreMs))
          liftIO (modifyIORef' c2cMsRef (+ coreMs))
          pure [(externalizeInternalTops simplified, mCapturedTy, mResultTy)]
        else pure []
    totalTcMs <- liftIO (readIORef tcMsRef)
    totalCoreMs <- liftIO (readIORef coreMsRef)
    liftIO (emitPhase timing "typecheck" totalTcMs)
    liftIO (emitPhase timing "core" totalCoreMs)
    -- Diagnostic-only (see 'dsMsRef'/'c2cMsRef' haddock above): NOT part of
    -- the tidepool-timing wire grammar, so 'ExtractTiming::parse' never sees
    -- it and there is nothing to keep in sync there.
    when timing $ liftIO $ do
      totalDsMs  <- readIORef dsMsRef
      totalC2cMs <- readIORef c2cMsRef
      let allModNames = [ m | (m, _, _, _, _) <- passOne ]
          moduleCount = length allModNames
          reachableCount = Set.size reachableMods
          validationOnly = [ moduleNameString m | m <- allModNames, not (m `Set.member` reachableMods) ]
      hPutStrLn stderr $
        "e6-tier modules=" ++ show moduleCount
        ++ " reachable=" ++ show reachableCount
        ++ " desugar_ms=" ++ show totalDsMs
        ++ " core2core_ms=" ++ show totalC2cMs
        ++ " validation_only=" ++ show validationOnly
        ++ " reachable_names=" ++ show (map moduleNameString (Set.toList reachableMods))
    -- Phase barrier (backstop): a target or dependency compile error already
    -- threw a spanned 'SourceError' from inside PASS 1 above (each summary's
    -- own 'parseModule'/'typecheckModule' redoes its typecheck independently
    -- of 'load'', so a real user type error surfaces there with its span
    -- intact) — this MUST run after both passes, not before, or that spanned
    -- diagnostic never fires and callers get this generic message instead.
    -- The phase timings above are emitted first, so a run that dies here still
    -- reports the work it did. Reaching here with 'loadFlag' still 'Failed'
    -- means the loop finished without re-surfacing whatever 'load'' choked on;
    -- stop rather than return a 'PipelineResult' built against a
    -- half-populated environment.
    case loadFlag of
      Failed    -> liftIO $ ioError $ userError $
        "runPipeline: module load failed compiling " ++ path
      Succeeded -> pure ()
    -- Merge: dependency module bindings first, target module last
    let isTargetMod g = moduleNameString (moduleName (mg_module g)) == targetModName
        fst3 (g, _, _) = g
        allGuts = map fst3 results
    (targetGuts, depGuts, capturedTy, resultTy) <- case filter (isTargetMod . fst3) results of
      ((tgt, ty, rty):_) -> return (tgt, [g | g <- allGuts, mg_module g /= mg_module tgt], ty, rty)
      []      -> liftIO $ ioError $ userError $
        "Target module '" ++ targetModName ++ "' not found among compiled modules: "
        ++ show (map (moduleNameString . moduleName . mg_module) allGuts)
    -- 'allTyCons' unconditionally covers EVERY compiled module, not just
    -- 'reachableMods': TyCon/DataCon declarations are populated by the
    -- typechecker and are never touched by 'core2core' (which transforms
    -- 'mg_binds' only — the two Passes above never re-derive 'mg_tcs'), so
    -- this costs nothing extra and keeps validation-only modules' data types
    -- available to D1's metadata walk exactly as before the tier.
    let allBinds = concatMap mg_binds depGuts ++ mg_binds targetGuts
        allTyCons = concatMap (\(_, _, g, _, _) -> mg_tcs g) passOne
    hscEnv <- getSession
    warnings <- liftIO (nub . reverse <$> readIORef warnRef)
    return PipelineResult
      { prBinds  = allBinds
      , prTyCons = allTyCons
      , prHscEnv = hscEnv
      , prCapturedType = capturedTy
      , prResultType   = resultTy
      , prWarnings     = warnings
      }

capitalize :: String -> String
capitalize [] = []
capitalize (c:cs) = toUpper c : cs

-- | A 'GHC.Utils.Logger.LogAction' hook that records every @SevWarning@
-- diagnostic whose source span is @targetPath@ (the file being extracted,
-- NOT a dependency module — the preamble/stdlib compile alongside it in the
-- same GHC session and must not leak their own warnings into the eval's).
-- Rendered with 'mkLocMessage', the same formatter GHC's default log action
-- uses, so the text carries the familiar @Expr.hs:<line>:<col>: warning:
-- ...@ shape callers already parse compile errors out of. Delegates to
-- `fallback` unconditionally so normal stderr printing is unaffected — this
-- only ADDS a capture, it never suppresses.
warnCollectorHook :: FilePath -> IORef [String] -> LogAction -> LogAction
warnCollectorHook targetPath ref fallback flags msgClass srcSpan msg = do
  case msgClass of
    MCDiagnostic SevWarning _ _ | inTarget srcSpan ->
      modifyIORef' ref (rendered :)
    _ -> pure ()
  fallback flags msgClass srcSpan msg
  where
    rendered = renderWithContext defaultSDocContext (mkLocMessage msgClass srcSpan msg)
    inTarget (RealSrcSpan rss _) = unpackFS (srcSpanFile rss) == targetPath
    inTarget _ = False

-- | The session-setup DynFlags transform shared by BOTH the normal and the
-- session paths, so the extracted Core is identical regardless of which entry
-- point is used: 'canonicalizeDFlags' + the genericPlatform spoof + exposing
-- the @ghc@ package + clearing host SIMD. Factored out (was inlined in
-- 'runNormalPipeline') purely to keep the two paths from drifting; the produced
-- 'DynFlags' is byte-for-byte what the normal path always built. See the long
-- commentary at the 'runNormalPipeline' call site for the rationale of each field.
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

-- | The SESSION extraction path (active 'SessionScope' only). It mirrors
-- 'runNormalPipeline' — @depanal@/@load'@ then a per-module
-- parse/typecheck/desugar/@core2core@ over every home module, returning all
-- their guts — with exactly two session-specific additions:
--
--   1. The source-less @Val.G<g>@ modules are EXCLUDED from @depanal@ (no source
--      to summarise) and their thin ifaces are INJECTED into the HPT + finder
--      ('injectSessionScope') so the turn target's @import Val.G<g>@ resolves.
--   2. The turn target is excluded from the phase-1 @load'@ (it cannot be
--      compiled before the Val ifaces are injected; see the body), but it IS
--      compiled in the phase-3 per-module loop after injection.
--
-- Compiling every home module to full -O2 guts (rather than extracting only the
-- target and resolving its library calls from HPT ifaces) is load-bearing: see
-- the PHASE 3 comment for why the iface-resolution shortcut bakes kind=4
-- ErrorSentinels. A reference turn imports @Tidepool.Prelude@ via the eval
-- preamble; the phase-1 @load'@ also keeps those source deps "loaded"
-- (GHC-58427).
runSessionPipeline :: SessionScope -> FilePath -> [FilePath] -> IO PipelineResult
runSessionPipeline scope path includes = do
  timing <- readTimingEnabled
  (libdir, startupMs) <- timeSection getLibdir
  emitPhase timing "startup" startupMs
  runGhc (Just libdir) $ do
    sessionT0 <- monotonicTime
    dflags <- getSessionDynFlags
    setSessionDynFlags (extractionDynFlags dflags includes)
    target <- guessTarget path Nothing Nothing
    setTargets [target]
    -- Success-path warning capture — see the identical install in
    -- 'runNormalPipeline' for why this must precede the per-module loop.
    warnRef <- liftIO (newIORef [])
    pushLogHookM (warnCollectorHook path warnRef)
    let targetModName = capitalize (takeBaseName path)
        -- The injected source-less @Val.G<g>@ modules: exclude from the
        -- downsweep (no source to summarise) — a deferred module's @import@ of
        -- them resolves from the HPT entry the injection registers in phase 2.
        excludedVal = map renderSessionModule (ssValIfaces scope)
    modGraphRaw <- depanal excludedVal False
    -- 'ghc_setup' phase (TIDEPOOL_TIMING): session DynFlags setup +
    -- guessTarget/setTargets + this 'depanal' call — SAME MEANING as the
    -- normal path's 'ghc_setup' phase. FLAT, partitioning what the retired
    -- 'ghc_session' bracket used to cover — see Tidepool.Timing's module
    -- haddock and the 'PHASE_GHC_SESSION' tombstone in timing.rs.
    setupT1 <- monotonicTime
    liftIO (emitPhase timing "ghc_setup" (elapsedMs sessionT0 setupT1))
    let unpoison ms =
          ms { ms_hspp_opts = gopt_unset (ms_hspp_opts ms) Opt_IgnoreInterfacePragmas }
        targetModName' = mkModuleName targetModName
        directSummaries = [ ms | ModuleNode _ ms <- mgModSummaries' modGraphRaw ]
        importsOf ms = [ unLoc lmn | (_, lmn) <- ms_textual_imps ms ]
        -- Everything that (directly or transitively) imports an injected
        -- Val module can't go through phase-1's @load'@ — its import can only
        -- resolve once phase 2's injection has happened. This generalizes the
        -- old "just exclude the target" rule: a decl module (@Lib.G<g>@) that
        -- itself imports a Val module is ALSO a dependency needing deferral,
        -- not just the ultimate leaf target. Plain forward fixpoint over the
        -- (small, per-turn) module graph — no existing GHC utility does this
        -- specific reverse-reachability query, so this is a self-contained
        -- graph closure over data already on each 'ModSummary'.
        closure seed =
          let grown = seed `Set.union` Set.fromList
                [ ms_mod_name ms
                | ms <- directSummaries
                , any (`Set.member` seed) (importsOf ms)
                ]
          in if grown == seed then seed else closure grown
        deferredMods = closure (Set.fromList (targetModName' : excludedVal))
        -- Exclude every deferred module (target ∪ transitive Val-importers)
        -- from the load' graph. A @load'@ that reaches one of them (e.g.
        -- @LoadDependenciesOf targetHUM@, whose @createBuildPlan@ includes ALL
        -- modules reachable from the root) compiles it BEFORE the Val iface is
        -- injected (PHASE 2), so its @import Tidepool.Session.Val.G<g>@ fails →
        -- GHC error-recovery emits "Could not find module" AND inserts a FAKE
        -- empty iface into the EPS PIT for the Val module. Filtering deferred
        -- modules out makes @load'@ compile ONLY the untouched source deps;
        -- each deferred module is compiled AND its interface registered back
        -- into the HPT in PHASE 3, post-injection, in dependency order.
        depGraph = mkModuleGraph
          [ node | node <- mgModSummaries' modGraphRaw
                 , case node of
                     ModuleNode _ ms -> not (ms_mod_name ms `Set.member` deferredMods)
                     _               -> True ]
    -- PHASE 1 — compile the turn's home-package SOURCE dependencies
    -- (@Tidepool.Prelude@, @Tidepool.Effects@, @Lib.G<g>@) into the HPT, but NOT
    -- the turn target itself. The target imports the source-less @Val@ modules
    -- (injected as ifaces in PHASE 2), so it cannot go through @load'@. We use
    -- LoadAllTargets on depGraph (target filtered out above) — equivalent to the
    -- old @LoadDependenciesOf@ but without compiling the target prematurely.
    loadT0 <- monotonicTime
    loadFlag <- load' Nothing LoadAllTargets
               mkUnknownDiagnostic (Just batchMsg) (mapMG unpoison depGraph)
    loadT1 <- monotonicTime
    -- 'ghc_load' phase (TIDEPOOL_TIMING): the 'load'' call alone, nothing
    -- else — same meaning as the normal path's 'ghc_load' phase. FLAT — see
    -- 'ghc_setup' above.
    liftIO (emitPhase timing "ghc_load" (elapsedMs loadT0 loadT1))
    -- Phase barrier: same policy as 'runNormalPipeline' — a 'Failed' PHASE 1
    -- dependency load stops here, before the module-graph restore, PHASE 2's
    -- Val iface injection, or PHASE 3's per-module compile ever see a
    -- half-populated HPT.
    case loadFlag of
      Failed    -> liftIO $ ioError $ userError $
        "runSessionPipeline: PHASE 1 dependency load failed compiling " ++ path
      Succeeded -> pure ()
    -- Restore the FULL module graph (target included) so PHASE 3's typecheck can
    -- see HPT instances from dep modules: @hptSomeThingsBelowUs@ walks
    -- @moduleGraphModulesBelow (hsc_mod_graph) target@, and @load'@ left
    -- @hsc_mod_graph = depGraph@ (target absent), which would yield an empty HPT
    -- instance env ("No instance for ToJSON …").
    do hscMG <- getSession
       setSession hscMG { hsc_mod_graph = modGraphRaw }
    -- PHASE 2 — inject the live @Val.G<g>@ ifaces into the now dep-populated
    -- HPT. AFTER @load'@, so its upsweep does not discard them; the subsequent
    -- per-module compile (no further @load'@) preserves them.
    injectT0 <- monotonicTime
    hsc0 <- getSession
    hscInjected <- injectSessionScope scope hsc0
    setSession hscInjected
    injectT1 <- monotonicTime
    -- 'inject' phase (TIDEPOOL_TIMING): PHASE 2's Val-iface injection alone.
    -- Session-path-only — the normal path never injects session Vals. FLAT,
    -- like every other phase here — not summed into anything.
    liftIO (emitPhase timing "inject" (elapsedMs injectT0 injectT1))
    -- PHASE 3 — compile EVERY home-source module (deps + target) to optimized
    -- Core, exactly like 'runNormalPipeline'. This is load-bearing: extracting
    -- only the target and resolving the home-library functions it calls
    -- (@object@, @.=@, @$fToJSONInt@, @toText@, …) from their HPT interface
    -- unfoldings does NOT work — @load'@ provisions those ifaces without -O2
    -- unfoldings, so 'resolveExternals' cannot inline them, bakes a poison
    -- ErrorSentinel for each, and the masking in 'translateModuleClosed'
    -- (@trulyUnresolved@, keyed on the un-poisoned id which never appears) hides
    -- it — the sentinel then fires at run as @kind=4 TypeMetadata@. Recompiling
    -- the deps here as full guts (the normal path's approach) gives their bodies
    -- directly, so no library function is ever left unresolved. The target's
    -- @import Val.G<g>@ resolves from the PHASE-2 injection.
    -- Dependency order matters now that MULTIPLE modules (not just one leaf
    -- target) may need deferred, post-injection compilation: a deferred
    -- module that itself depends on another deferred module (e.g. the target
    -- importing a Val-referencing @Lib.G<g>@) must see the latter ALREADY
    -- reinserted into the HPT by the time its own turn in this loop comes up.
    -- @mgModSummaries@/@mg_mss@ is not guaranteed topologically ordered (see
    -- its haddock); @topSortModuleGraph@ + @flattenSCCs@ (both re-exported by
    -- the umbrella 'GHC' module already imported here) give a real
    -- deps-before-dependents order.
    let summaries =
          [ ms | ModuleNode _ ms <- flattenSCCs (topSortModuleGraph True modGraphRaw Nothing)
               -- Same hs-boot exclusion as 'runNormalPipeline' (item 20):
               -- boot guts carry no bindings and must not be desugared or
               -- merged as if they were the real module.
               , ms_hsc_src ms == HsSrcFile ]
    when (null summaries) $
      liftIO $ ioError (userError "runSessionPipeline: empty module graph")
    tcMsRef   <- liftIO (newIORef (0 :: Integer))
    coreMsRef <- liftIO (newIORef (0 :: Integer))
    results <- forM summaries $ \modSum0 -> do
      let modSum = modSum0 { ms_hspp_opts = canonicalizeDFlags (ms_hspp_opts modSum0) }
      -- 'typecheck'/'core' are summed ACROSS this loop (one line each,
      -- emitted after) exactly like 'runNormalPipeline' — same rationale:
      -- the wire grammar is one line per phase per process, and a turn
      -- module always compiles alongside its preamble/stdlib dep modules in
      -- the same loop.
      (typechecked, tcMs) <- timeSection $ do
        parsed <- parseModule modSum
        typecheckModule parsed
      liftIO (modifyIORef' tcMsRef (+ tcMs))
      hscEnv0     <- getSession
      let hscEnv   = hscUpdateFlags canonicalizeDFlags hscEnv0
          tcGblEnv = fst (tm_internals_ typechecked)
          mCapTy   = capturedUserType tcGblEnv
          -- Reached only once a session has a prior binding to inject
          -- ('isSessionScopeActive'); every such turn's wrapper compiles a
          -- target literally named @__result@ (scaffold-reserved, never
          -- @result@ — see 'processSessionFile').
          mResTy   = capturedBindingType "__result" tcGblEnv
      (simplified, coreMs) <- timeSection $ do
        desugared <- liftIO $ hscDesugar hscEnv modSum tcGblEnv
        liftIO $ core2core hscEnv desugared
      liftIO (modifyIORef' coreMsRef (+ coreMs))
      -- A deferred module (target ∪ transitive Val-importers, computed above)
      -- was deliberately excluded from PHASE 1's @load'@, so nothing has
      -- registered it in the HPT yet — do that here, now that PHASE 2's Val
      -- injection has happened, so a LATER module in this same loop that
      -- imports this one (e.g. the leaf importing a Val-referencing
      -- @Lib.G<g>@) can resolve it. Real 'ModIface'/'ModDetails' via the same
      -- tidy→iface pipeline GHC's own batch compiler uses internally
      -- ('hscTidy' wraps 'initTidyOpts'+'tidyProgram'; 'mkIfaceTc' is what
      -- 'hscSimpleIface'' uses for "a stripped down interface... where we
      -- aren't generating any object code at all" — precisely this case,
      -- since Core is extracted separately for the Cranelift JIT and nothing
      -- here ever executes via GHC's own bytecode interpreter, hence no real
      -- linkable is ever needed — 'emptyHomeModInfoLinkable' is the same
      -- legitimate "no linkable" value GHC itself uses for @.hs-boot@
      -- modules). Mirrors 'upsweep_mod's own @addToHpt@ call.
      when (ms_mod_name modSum `Set.member` deferredMods) $ do
        (cgGuts, modDetails) <- liftIO $ hscTidy hscEnv simplified
        iface <- liftIO $
          mkIfaceTc hscEnv Sf_None modDetails modSum (Just (cg_binds cgGuts)) tcGblEnv
        let hmi = HomeModInfo iface modDetails emptyHomeModInfoLinkable
        hscEnvNow <- getSession
        setSession (hscUpdateHPT (\hpt -> addToHpt hpt (ms_mod_name modSum) hmi) hscEnvNow)
      return (externalizeInternalTops simplified, mCapTy, mResTy)
    totalTcMs   <- liftIO (readIORef tcMsRef)
    totalCoreMs <- liftIO (readIORef coreMsRef)
    liftIO (emitPhase timing "typecheck" totalTcMs)
    liftIO (emitPhase timing "core" totalCoreMs)
    let isTargetMod g = moduleNameString (moduleName (mg_module g)) == targetModName
        fst3 (g, _, _) = g
        allGuts = map fst3 results
    (targetGuts, depGuts, capturedTy, resultTy) <- case filter (isTargetMod . fst3) results of
      ((tgt, ty, rty):_) -> return (tgt, [g | g <- allGuts, mg_module g /= mg_module tgt], ty, rty)
      []      -> liftIO $ ioError $ userError $
        "runSessionPipeline: target module '" ++ targetModName ++ "' not found among: "
        ++ show (map (moduleNameString . moduleName . mg_module) allGuts)
    let allBinds  = concatMap mg_binds depGuts ++ mg_binds targetGuts
        allTyCons = concatMap mg_tcs depGuts ++ mg_tcs targetGuts
    hscFinal <- getSession
    warnings <- liftIO (nub . reverse <$> readIORef warnRef)
    return PipelineResult
      { prBinds        = allBinds
      , prTyCons       = allTyCons
      , prHscEnv       = hscUpdateFlags canonicalizeDFlags hscFinal
      , prCapturedType = capturedTy
      , prResultType   = resultTy
      , prWarnings     = warnings
      }

-- | Read the inferred type of the @__user@ binding out of a module's
-- typechecked type env and render it to a (re-injectable) string.
--
-- @__user@ is the binder the eval template wraps the user's expression in
-- (eval_prep.rs); its 'idType' is exactly the type of the eval's top-level
-- expression. We render with the same 'renderWithContext'/'ppr' pattern as
-- 'dumpCore'. 'Nothing' when no such binder exists (non-eval extractions like
-- the test Suite have no @__user@).
capturedUserType :: TcGblEnv -> Maybe String
capturedUserType tcg =
  case [ i | i <- typeEnvIds (tcg_type_env tcg)
           , occNameString (nameOccName (idName i)) == "__user" ] of
    (i:_) -> Just (renderWithContext defaultSDocContext (ppr (idType i)))
    []    -> Nothing

-- | Read the GHC 'Type' (NOT a rendered string) of the named top-level binding
-- out of a module's typechecked type env. Used by the Wave-3b BIND mode to grab
-- the @result@ binding's @Eff stack T@ type so 'stripMonadHead' can recover the
-- bound value's type @T@ for the thin session iface + the BoundBinder sidecar.
-- 'Nothing' when no such binder exists (every non-bind extraction).
capturedBindingType :: String -> TcGblEnv -> Maybe Type
capturedBindingType occ tcg =
  case [ i | i <- typeEnvIds (tcg_type_env tcg)
           , occNameString (nameOccName (idName i)) == occ ] of
    (i:_) -> Just (idType i)
    []    -> Nothing

-- | Strip a monadic head off a bind target's type: @Eff stack T@ → @T@,
-- @M a@ → @a@. Drops any leading foralls/constraint context first
-- ('tcSplitSigmaTy'), then peels the last type application ('splitAppTy_maybe')
-- — for @Eff es a@ that is @(Eff es) a@, yielding @a@. A non-application body
-- (a nullary type) is returned unchanged.
stripMonadHead :: Type -> Type
stripMonadHead ty =
  let (_, _, body) = tcSplitSigmaTy ty
  in case splitAppTy_maybe body of
       Just (_, res) -> res
       Nothing       -> body

-- | Is the bound value a CLOSURE (Tier1) rather than first-order data (Tier0)?
-- True iff @T@ is a function type after stripping its own foralls/context — the
-- distinction the bind path uses to decide strict-force (Tier0) vs store-as-is
-- (Tier1).
isClosureType :: Type -> Bool
isClosureType ty = let (_, _, body) = tcSplitSigmaTy ty in isFunTy body

-- | Split a tuple type into its component types. @(T1, T2, ..., Tn)@ → @Just
-- [T1, T2, ..., Tn]@. Returns @Nothing@ for non-tuple types (constructors,
-- newtypes, function types, etc.). Used by the multi-binder bind path to verify
-- that an N-name bind has an N-tuple return type.
splitTupleType :: Type -> Maybe [Type]
splitTupleType ty =
  let (_, _, body) = tcSplitSigmaTy ty
  in case splitTyConApp_maybe body of
       Just (tc, args) | isTupleTyCon tc -> Just args
       _                                  -> Nothing

-- | Render a 'Type' to a display string (for the @typeDisplay@ field / @:t@),
-- the same 'ppr' pattern as 'capturedUserType' / 'dumpCore'.
renderType :: Type -> String
renderType ty = renderWithContext defaultSDocContext (ppr ty)

-- | E6's reachable-module rule: a home module is REACHABLE from @target@ iff
-- it IS @target@, or its (desugared) Core is transitively referenced — via a
-- real 'Var' occurrence at any depth — from @target@'s own desugared Core.
-- See the call site in 'runNormalPipeline' for the full soundness argument
-- (why pre-'core2core' desugared Core is the right stage, and why computing
-- this any earlier — e.g. from renamer/typecheck-level "used name" tracking
-- — would be UNSOUND: it would miss a module imported only for an orphan
-- instance).
reachableModuleClosure :: ModuleName -> Map.Map ModuleName ModGuts -> Set.Set ModuleName
reachableModuleClosure target gutsByMod = go (Set.singleton target) [target]
  where
    known = Map.keysSet gutsByMod
    go visited [] = visited
    go visited (m:ms) = case Map.lookup m gutsByMod of
      Nothing   -> go visited ms
      Just guts ->
        let refs = moduleRefs known guts
            new  = refs `Set.difference` visited
        in go (visited `Set.union` new) (ms ++ Set.toList new)

-- | Every OTHER home module (restricted to @known@) a module's top-level
-- binding RHSs reference, via 'externalVarModules'.
moduleRefs :: Set.Set ModuleName -> ModGuts -> Set.Set ModuleName
moduleRefs known guts = Set.unions (map rhsModules (mg_binds guts))
  where
    rhsModules (NonRec _ rhs) = externalVarModules known rhs
    rhsModules (Rec ps)       = Set.unions [ externalVarModules known rhs | (_, rhs) <- ps ]

-- | Every home module (restricted to @known@) referenced by a real 'Var'
-- occurrence anywhere in a Core expression, at any binding depth. No
-- bound-variable tracking needed, unlike 'Translate.exprFreeVarKeys': a Core
-- 'Var' occurrence already points at its exact binder 'Id' (resolved by the
-- renamer/typechecker), so a local binder can never be confused with an
-- unrelated same-named import the way source text could — this walk cannot
-- under- OR over-count due to shadowing. Over-inclusion elsewhere (e.g. a
-- 'Var' for a DataCon worker, whose own defining module needs no -O2
-- unfoldings to be useful — its representation comes from static DataCon
-- info, always available regardless of tier) is harmless: the only failure
-- mode this item must avoid is EXCLUDING a module the target's Core
-- genuinely needs; including one too many only gives back some of the tier's
-- win, never correctness.
externalVarModules :: Set.Set ModuleName -> CoreExpr -> Set.Set ModuleName
externalVarModules known = go
  where
    go expr = case expr of
      Var v -> case nameModule_maybe (idName v) of
        Just m | moduleName m `Set.member` known -> Set.singleton (moduleName m)
        _ -> Set.empty
      Lit _           -> Set.empty
      App f a         -> go f `Set.union` go a
      Lam _ e         -> go e
      Let b e         -> bindRefs b `Set.union` go e
      Case s _ _ alts -> go s `Set.union` Set.unions [ go rhs | Alt _ _ rhs <- alts ]
      Cast e _        -> go e
      Tick _ e        -> go e
      Type _          -> Set.empty
      Coercion _      -> Set.empty
    bindRefs (NonRec _ rhs) = go rhs
    bindRefs (Rec ps)       = Set.unions [ go rhs | (_, rhs) <- ps ]

-- | The canonical extraction DynFlags transformation, applied to the session
-- flags at startup AND re-applied per-module before extraction.
--
-- Why re-applied: GHC 9.12's @enableCodeGenForTH@ downgrades the DynFlags of
-- home modules whose code is needed for splices (QuasiQuotes/TH) so 'load'
-- can provision bytecode — interpreter backend, -O0. That downgrade is
-- correct for the load phase (splices run against dep bytecode), but it
-- persists in each ModSummary's @ms_hspp_opts@, which the extraction loop
-- re-uses. Without re-canonicalizing, extraction of any module in a
-- quasi-quote dependency graph emits UNOPTIMIZED Core — e.g.
-- @negate \@Double $fNumDouble (D# 2.5##)@ instead of a folded @D# -2.5##@,
-- which then chases Integer machinery and dies with
-- "Unsupported primop: clz#". Repro matrix M1-M8 lives in scratch/qq-spike/.
--
-- Surgical: backend/opt-level/gopt only — exactly the fields the TH
-- downgrade touches. Per-module LANGUAGE pragmas already merged into
-- @ms_hspp_opts@ are preserved. Platform spoofing and @importPaths@ are
-- session-setup-only (see runPipeline): re-pinning bare genericPlatform
-- here would strip the platform constants populated at session init.
--
-- Flag notes (history, do not weaken):
--   * FullLaziness conflicts with eager eval.
--   * WARNING (2026-06-10, #313 forensics): Opt_CprAnal is a NO-OP in GHC
--     9.12 — `-fno-cpr-anal` changes nothing (Cpr=1 signatures appear
--     regardless; verified empirically). The unset is kept for
--     documentation, but the protection it was believed to provide does
--     not exist. Disabling Opt_WorkerWrapper was tried and did NOT fix
--     #313 (the bug is join-closure wiring in translation, not w/w), so
--     it stays enabled.
--   * Opt_ShowErrorContext / maxRelevantBinds (this change): every repl/eval
--     turn typechecks the user's expression inside harness scaffolding
--     (@__user@, @__b@, @it@, @toWire@ wrapper bindings). An ambiguity in
--     user code cascades into fallout against that wrapper, and GHC's
--     default renderer appends "In the expression: toWire it / In a stmt
--     of a 'do' block: …" context trails and "Relevant bindings include
--     __b :: f0 (Text, Int) …" lists that name scaffold identifiers the
--     caller never wrote and never asked about — pure noise, unlike
--     hole-fits below (which the caller DID ask about, via a literal `_`).
--     Opt_ShowErrorContext off drops the context trail entirely;
--     maxRelevantBinds = Just 0 drops (or minimizes, GHC may print a
--     "(Some bindings suppressed …)" stub) the relevant-bindings list.
canonicalizeDFlags :: DynFlags -> DynFlags
canonicalizeDFlags dflags =
  -- Trim machine-channel noise: typed-hole "Valid hole fits include …" lists
  -- are enormous (dozens of candidates) and useless to an LLM caller; the
  -- "Perhaps you meant …" similar-name hints are a separate mechanism and stay.
  -- -fprefer-byte-code (session-wide): when enableCodeGenForTH must provision
  -- a splice's home-module dependencies, provision them as BYTECODE, not native
  -- object code. Set at session init (NOT just per-summary) so GHC's downsweep
  -- — which re-derives each module's backend inside 'load' and ignores a
  -- backend field we patch onto a summary afterwards — chooses the interpreter.
  -- Object-code provisioning emits a .s and shells to the assembler; under the
  -- genericPlatform spoof that .s is x86_64/ELF and the macOS Mach-O assembler
  -- rejects it (`.type …, @object`; x86 mnemonics on aarch64). Bytecode is
  -- architecture-neutral, so the spoof stays confined to extracted Core while
  -- splices run host-agnostically. (Was the aarch64-darwin assembler failure
  -- that broke every eval on Apple Silicon.)
  (`gopt_set` Opt_UseBytecodeRatherThanObjects) $
  -- Valid-hole-fits stay ON: with ~200 stdlib/verb names in scope, "fits"
  -- on a typed hole is the interface's vocabulary-discovery engine (an LLM
  -- writes `_` to ask "what goes here"). The search only runs on hole
  -- errors, never on clean compiles.
  (`gopt_unset` Opt_ShowErrorContext) $
  gopt_set (gopt_set (gopt_unset (gopt_unset (updOptLevel 2 $ dflags
        { backend = noBackend
        , ghcLink = NoLink
        , maxRelevantBinds = Just 0
        }) Opt_FullLaziness) Opt_CprAnal)
        Opt_ExposeAllUnfoldings) Opt_ExposeOverloadedUnfoldings

-- | #313 fix: disambiguate top-level simplifier floats across modules.
--
-- Top-level binders with INTERNAL names (floats like @k_X1@, @$wk_snOX@) keep
-- per-module uniques. `runPipeline` concatenates several modules' bindings for
-- translation, so (occName, unique-key) pairs collide across modules — and
-- @Translate.localVarId@ hashes exactly that pair. Two distinct floats can
-- then receive the same VarId and shadow each other in the serialized program.
-- Observed as #313: Probe's tuple-unpacking continuation @k_X1@ resolved to
-- the preamble's unrelated @k_X1 :: [Text] -> ...@, sending the raw effect
-- tuple into a list case → CASE TRAP.
--
-- Fix: give every internal top-level binder an EXTERNAL name qualified by its
-- defining module, with the unique key baked into the OccName
-- (@k@ → @Probe.k_u8214565720323785735@), so @Translate.stableVarId@ yields a
-- globally unique, deterministic VarId. Internal names cannot be referenced
-- from other modules' ModGuts, so substituting binder + occurrences within the
-- module is complete. Nested binders are untouched: their uniques cannot
-- collide with top-level uniques of the same module, and cross-module nested
-- references are lexically impossible.
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
    -- Substitute occurrences only; nested binders keep their names.
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

dumpCore :: [CoreBind] -> String
dumpCore binds = renderWithContext defaultSDocContext (pprCoreBindings binds)

getLibdir :: IO FilePath
getLibdir = do
  envDir <- lookupEnv "TIDEPOOL_GHC_LIBDIR"
  case envDir of
    Just dir -> pure dir
    Nothing  -> trim <$> readProcess "ghc" ["--print-libdir"] ""
  where trim = reverse . dropWhile (== '\n') . reverse
