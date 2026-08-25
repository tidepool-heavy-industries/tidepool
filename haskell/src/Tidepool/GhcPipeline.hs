module Tidepool.GhcPipeline
  ( runPipeline, runPipelineSession, PipelineResult(..), dumpCore
    -- * Bound-value type analysis (Wave 3b BIND mode)
  , stripMonadHead, isClosureType, renderType
  , splitTupleType
    -- * Batch turns (plans/post-restart/batch-turns-feasibility.md §8)
  , BatchItem(..), BatchItemResult(..), runBatchPipeline
    -- * Resident session (plans/compile-daemon-design.md, Phase 0)
  , withResidentPipeline
  ) where

import GHC
import GHC.Hs (hsmodDecls)
import GHC.Driver.Main (hscDesugar, batchMsg, hscTidy)
import GHC.Driver.Env (hscUpdateFlags, hscUpdateHPT)
import GHC.Driver.Env.Types (HscEnv(hsc_mod_graph))
import GHC.Driver.Monad (reflectGhc, reifyGhc)
import GHC.Unit.Home (homeUnitId)
import GHC.Unit.Home.ModInfo (HomeModInfo(..), emptyHomeModInfoLinkable, addToHpt)
import GHC.Driver.Make (load', ModIfaceCache, newIfaceCache)
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
  , WarningFlag
      ( Opt_WarnMissingFields
      , Opt_WarnIncompletePatterns
      , Opt_WarnIncompleteUniPatterns
      )
  , wopt_set, wopt_set_fatal
  , packageFlags, PackageFlag(..), PackageArg(..), ModRenaming(..) )
import GHC.Unit.Module.ModGuts (ModGuts(..), CgGuts(..))
import GHC.Core (CoreBind, CoreExpr, Bind(..), Expr(..), Alt(..))
import qualified Data.Set as Set
import qualified Data.Map.Strict as Map
import GHC.Platform (genericPlatform)
import GHC.Utils.Outputable (renderWithContext, defaultSDocContext, ppr)
import GHC.Types.Id (idName, idType)
import GHC.Core.Type (splitAppTy_maybe, splitTyConApp_maybe, splitFunTy_maybe)
import GHC.Core.TyCon (isTupleTyCon, tyConDataCons_maybe, unwrapNewTyCon_maybe, tyConUnique)
import GHC.Builtin.Names (fUNTyConKey, unrestrictedFunTyConKey)
import GHC.Core.DataCon (dataConOrigArgTys)
import GHC.Core.TyCo.Rep (Scaled(..))
import GHC.Types.Unique.Set (UniqSet, emptyUniqSet, addOneToUniqSet, elementOfUniqSet)
import GHC.Tc.Utils.TcType (tcSplitSigmaTy)
import GHC.Types.TypeEnv (typeEnvIds)
import GHC.Tc.Types (TcGblEnv, tcg_type_env)
import GHC.Types.Name (nameOccName, nameUnique, mkExternalName, nameModule_maybe)
import GHC.Types.Name.Occurrence (mkOccName, occNameSpace, occNameString)
import GHC.Types.Var (setVarName)
import GHC.Types.Var.Env (mkVarEnv, lookupVarEnv)
import Control.Applicative ((<|>))
import Control.Exception (SomeException, try)
import Data.Maybe (fromMaybe, isNothing)
import Data.List (nub, sortOn)
import Data.IORef (IORef, newIORef, modifyIORef', readIORef)
import System.Environment (lookupEnv)
import System.FilePath (takeBaseName)
import System.IO (hPutStrLn, stderr)
import Control.Monad.IO.Class (liftIO)
import Control.Monad (forM, when)
import Tidepool.ExtractUtil (getLibdir, capitalize)
import Tidepool.Session
  ( SessionScope(..), isSessionScopeActive, injectSessionScope, renderSessionModule
  , scaffoldTargetName, scaffoldOutputBase, evalUserBinder, parseSessionModule )
import Tidepool.Timing
  ( readTimingEnabled, timeSection, emitPhase, monotonicTime, elapsedMs
  , emitCompileSummary, emitModuleTiming )
import Tidepool.Binders (ExportItem, declItems)

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

-- ---------------------------------------------------------------------------
-- The shared compile loop and its two seams
--
-- There is exactly ONE compile skeleton ('runCompileCycle'): session setup,
-- @depanal@, @load'@, the hs-boot summary filter, the per-module
-- parse/typecheck/capture/desugar/@core2core@, the summed timing phases, and
-- the guts→'PipelineResult' merge. The normal and session pipelines are that
-- skeleton plus a 'PipelineVariant'; 'runCompile' is a thin per-session
-- bootstrap around a single 'runCompileCycle' call, and 'runBatchPipeline'
-- bootstraps once and calls it N times (see 'runCompileCycle''s own haddock
-- for that seam). See plans/post-restart/ghcpipeline-seam-analysis.md for the
-- line-by-line classification the 'PipelineVariant' factoring came out of,
-- and for why the two seams below are the only genuine ones.
-- ---------------------------------------------------------------------------

-- | Which modules pay 'core2core' — and, inseparably, in what SCHEDULE the
-- per-module loop runs. The two are one seam, not two: a tier rule that needs
-- a global view of every module's Core forces staging, and only a variant that
-- optimizes everything is free to interleave.
data TierPolicy
  = OptimizeEveryModule
    -- ^ 'core2core' every module, INTERLEAVED: each module runs
    -- typecheck→desugar→'core2core'→'cpAfterModule' before the next module
    -- starts. The session path requires this ordering — its 'cpAfterModule'
    -- registers a deferred module's iface in the HPT, and a LATER deferred
    -- module's TYPECHECK resolves its @import@ out of that entry.
  | OptimizeCoreReachable
    -- ^ E6 (tiered -O2): STAGE the loop — parse/typecheck/desugar every
    -- module first, then run 'core2core' (canonicalizeDFlags' -O2 + exposed
    -- unfoldings) only for the target and its Core-reachable dependencies.
    -- Staging is forced by the rule itself: 'reachableModuleClosure' is
    -- computed over EVERY module's desugared Core, so no module's tier is
    -- known until all desugars have run. A module outside the closure still
    -- gets parsed/typechecked (its diagnostics still surface) and desugared
    -- (needed to compute reachability at all), but never pays 'core2core'.

-- | A pipeline variant: everything the shared skeleton cannot decide for
-- itself. 'pvPlan' runs after @depanal@ (it needs the downsweep graph) and
-- before @load'@.
data PipelineVariant = PipelineVariant
  { pvLabel :: String
    -- ^ Prefix on this variant's own error messages.
  , pvDownsweepExcludes :: [ModuleName]
    -- ^ Modules @depanal@ must NOT try to summarise (the session path's
    -- source-less @Val.G\<g\>@ ifaces). Empty on the normal path.
  , pvPlan :: Bool -> ModuleGraph -> Ghc CompilePlan
    -- ^ @pvPlan timingEnabled downsweepGraph@.
  }

-- | The seam values for one run, derived from the downsweep graph.
data CompilePlan = CompilePlan
  { cpLoadGraph :: ModuleGraph
    -- ^ The graph handed to @load'@ (the skeleton applies @unpoison@ itself).
  , cpAfterLoad :: SuccessFlag -> Ghc ()
    -- ^ Runs immediately after @load'@ and its @ghc_load@ phase emit, before
    -- summaries are taken. The session path puts its PHASE-1 load barrier,
    -- module-graph restore, Val-iface injection and @inject@ phase here.
  , cpSummaries :: Ghc [ModSummary]
    -- ^ The modules to compile, in compile ORDER, BEFORE the hs-boot filter
    -- (which is the skeleton's, at one site).
  , cpResultBinders :: [String]
    -- ^ OccNames to try, in order, for 'prResultType' — the @result@ vs
    -- @__result@ convention, which differs by wrapper.
  , cpAfterModule :: ModSummary -> TcGblEnv -> HscEnv -> ModGuts -> Ghc ()
    -- ^ Runs after a module's 'core2core', on the pre-'externalizeInternalTops'
    -- guts. The session path registers deferred modules into the HPT here.
  , cpTier :: TierPolicy
  , cpBeforeMerge :: SuccessFlag -> Ghc ()
    -- ^ Runs after the compile loop and its phase emits, before the guts are
    -- merged. The normal path puts its load barrier here (deliberately LATE —
    -- see 'normalVariant').
  , cpFinalEnv :: HscEnv -> HscEnv
    -- ^ Applied to the post-loop session before it becomes 'prHscEnv'.
  }

-- | One module's front half: everything produced by parse/typecheck/desugar,
-- carried to the back half ('core2core') whether that runs immediately
-- ('OptimizeEveryModule') or in a second stage ('OptimizeCoreReachable').
data ModuleFront = ModuleFront
  { mfSummary    :: ModSummary
  , mfHscEnv     :: HscEnv
  , mfTcGblEnv   :: TcGblEnv
  , mfDesugared  :: ModGuts
  , mfUserType   :: Maybe String
  , mfResultType :: Maybe Type
  }

runCompile :: PipelineVariant -> FilePath -> [FilePath] -> IO PipelineResult
runCompile variant path includes = do
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
    dflags' <- liftIO (withBuildProductsFromEnv (extractionDynFlags dflags includes))
    setSessionDynFlags dflags'
    -- One cycle, no cache, no memo — 'sessionT0' is captured BEFORE this
    -- DynFlags bootstrap (above) so the default-on per-compile summary's
    -- wall-clock figure covers it too, exactly as it always has. See
    -- 'runCompileCycle''s haddock for what each argument controls.
    runCompileCycle Nothing Nothing timing (Just sessionT0) variant path

-- | Extraction with optional tidepool-repl SESSION scope (Option-C type plane).
--
-- @Nothing@ (or an inert 'SessionScope') → 'normalVariant', the ordinary
-- @depanal@/@load@ downsweep path. @Just@ an ACTIVE scope → 'sessionVariant',
-- which injects the live session @Val.G<g>@ ifaces into the HPT and compiles
-- every home module. Both run the SAME 'runCompile' skeleton; the variant is
-- the only difference.
--
-- The gate is the guard below: the session arm runs ONLY for an active scope.
runPipelineSession :: Maybe SessionScope -> FilePath -> [FilePath] -> IO PipelineResult
runPipelineSession mscope path includes
  | Just scope <- mscope, isSessionScopeActive scope =
      runCompile (sessionVariant scope path) path includes
  | otherwise = runCompile (normalVariant path) path includes

-- ---------------------------------------------------------------------------
-- Batch turns (plans/post-restart/batch-turns-feasibility.md §8): N item
-- compiles in ONE GHC session, threading GHC's own 'ModIfaceCache' (§7.1 —
-- cycles 2..N skip stdlib recompilation) and a per-module dep-guts memo
-- (§7.6/§7.3 — a module's guts, once compiled in ANY cycle, are reused
-- verbatim by every LATER cycle that compiles the same module again). The
-- memo is populated INCREMENTALLY — on a module's FIRST compile in the
-- batch, whichever cycle that is — which is what closes §7.6's
-- incremental-population gap: a module that only appears partway through the
-- batch still gets memoized from that point on, without needing to be known
-- up front.
--
-- 'runCompileCycle' is the ONE compile-cycle body, shared by 'runCompile'
-- (called once, 'mCache'/'mMemoRef' both 'Nothing') and 'runBatchPipeline'
-- (called N times against one already-bootstrapped session, both threaded) —
-- see its own haddock for what each of its seams controls. The normal and
-- single-turn session paths (both via 'runCompile') are pinned byte-identical
-- by 'cross_mode_targeted'; the batch path is pinned by
-- @extract-fidelity-test@'s 'Fidelity.TurnBatch' (per-item error attribution,
-- and item 0's output being byte-identical-single-turn shape).
-- ---------------------------------------------------------------------------

-- | One memoized module's compile artifacts, keyed by 'ModuleName' across a
-- batch's cycles. A module's SOURCE cannot change within one batch spawn (a
-- stdlib/library module is stable; a batch item's own turn module is always
-- freshly, uniquely named by the caller — see app/Main.hs's per-item module
-- renaming), so once compiled in ANY cycle its guts are valid for every later
-- cycle that sees the same module name again.
data GutsMemoEntry = GutsMemoEntry
  { gmeFront      :: ModuleFront
    -- ^ For 'allTyCons' (TyCons never change across cycles — 'core2core'
    -- transforms 'mg_binds' only, see the comment at 'runCompileCycle''s own
    -- 'allTyCons' computation).
  , gmeSimplified :: ModGuts
    -- ^ Post-'core2core', PRE-'externalizeInternalTops' — needed to redo
    -- 'cpAfterModule''s HPT (re-)registration on a later cycle: 'load''
    -- clears the whole HPT on every call (§7.1), so a module 'cpAfterModule'
    -- deferred and hand-registered in an EARLIER cycle needs that
    -- registration REDONE (cheaply — no recompilation, just 'hscTidy' +
    -- 'mkIfaceTc' over already-computed guts) whenever it is deferred again
    -- in a LATER cycle.
  , gmeResult     :: (ModGuts, Maybe String, Maybe Type)
    -- ^ Post-externalize triple, exactly the shape 'results' carries.
  }

type GutsMemo = Map.Map ModuleName GutsMemoEntry

-- | The ONE compile-cycle body: session setup (target/@depanal@/@load'@), the
-- hs-boot summary filter, the per-module
-- parse/typecheck/capture/desugar/@core2core@ loop, the summed timing
-- phases, and the guts→'PipelineResult' merge. Runs inside an
-- ALREADY-OPEN 'Ghc' session with 'DynFlags' already set — the caller
-- ('runCompile' for a lone compile, 'runBatchPipeline' for N compiles sharing
-- one session) owns that bootstrap, since a batch sets it up exactly ONCE
-- across every cycle (mirroring the proven spike shape —
-- @runScenario@/@runGutsMemoScenario@ in @spike-batch/Spike.hs@:
-- @setSessionDynFlags@ outside the per-cycle loop).
--
-- Three seams, independent of the 'PipelineVariant' seam above:
--
--   * 'mCache' — 'load''s 'ModIfaceCache' (§7.1 — cycles 2..N skip stdlib
--     recompilation). 'Nothing' matches a lone compile's own
--     @load' Nothing ...@ byte for byte.
--   * 'mMemoRef' — the per-module dep-guts memo (§7.6/§7.3 — a module's
--     guts, once compiled in ANY cycle, are reused verbatim by every LATER
--     cycle that compiles the same module again). 'Nothing' disables it
--     entirely, compiling every module fresh — a lone compile's only cycle
--     always takes this path.
--   * 'mSummaryT0' — 'Just' the wall-clock time the CALLER considers the
--     compile's start (captured before the caller's own session bootstrap,
--     so the default-on per-compile summary's wall time includes it, exactly
--     as it always has) triggers 'emitCompileSummary'/'emitModuleTiming' at
--     the end of this cycle. 'Nothing' captures a fresh start time for this
--     cycle's OWN @ghc_setup@ phase and skips the summary entirely — a
--     batch's per-item cycles report through its own stdout document instead
--     (see @app/Main.hs@'s @--turn-batch@ mode), not a per-cycle summary.
--
-- Per-module wall time ('moduleMsRef') is tracked UNCONDITIONALLY regardless
-- of 'mSummaryT0' — cheap monotonic-clock reads, like 'tcMsRef'/'coreMsRef'
-- below, and costing nothing observable on a batch cycle that never reads it
-- back; the summary needs a top-3 to report whenever it does fire.
runCompileCycle
  :: Maybe ModIfaceCache -> Maybe (IORef GutsMemo)
  -> Bool -> Maybe Double -> PipelineVariant -> FilePath -> Ghc PipelineResult
runCompileCycle mCache mMemoRef timing mSummaryT0 variant path = do
    sessionT0 <- maybe monotonicTime pure mSummaryT0
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
    modGraphRaw <- depanal (pvDownsweepExcludes variant) False
    -- 'ghc_setup' phase (TIDEPOOL_TIMING): 'guessTarget'/'setTargets' + this
    -- 'depanal' call, nothing else, on EVERY caller — a lone compile also
    -- includes its own (one-time-per-call) session 'DynFlags' bootstrap in
    -- this window, since 'sessionT0' above was captured before it (see
    -- 'runCompile'); a batch cycle's session 'DynFlags' are set ONCE, before
    -- ANY cycle (see 'runBatchPipeline'), so that one-time cost falls
    -- outside every cycle's own 'ghc_setup' window. FLAT and non-overlapping
    -- with 'ghc_load' below — see Tidepool.Timing's module haddock. SAME
    -- MEANING across every 'PipelineVariant' (normal vs. session).
    setupT1 <- monotonicTime
    liftIO (emitPhase timing "ghc_setup" (elapsedMs sessionT0 setupT1))
    plan <- pvPlan variant timing modGraphRaw
    -- unpoison: keep the EPS healthy under the TH/QQ downgrade by unsetting
    -- Opt_IgnoreInterfacePragmas on every summary (see the depanal/load'
    -- haddock above). The bytecode-vs-object provisioning choice is made
    -- session-wide in canonicalizeDFlags (Opt_UseBytecodeRatherThanObjects) —
    -- it has to be set before downsweep, since 'load' re-derives each module's
    -- backend and ignores a field patched onto a summary here.
    let unpoison ms =
          ms { ms_hspp_opts = gopt_unset (ms_hspp_opts ms) Opt_IgnoreInterfacePragmas }
    loadT0 <- monotonicTime
    loadFlag <- load' mCache LoadAllTargets mkUnknownDiagnostic (Just batchMsg)
               (mapMG unpoison (cpLoadGraph plan))
    loadT1 <- monotonicTime
    -- 'ghc_load' phase (TIDEPOOL_TIMING): the 'load'' call alone, nothing
    -- else. FLAT — see 'ghc_setup' above; the two rows partition the work,
    -- they do not nest inside each other.
    liftIO (emitPhase timing "ghc_load" (elapsedMs loadT0 loadT1))
    cpAfterLoad plan loadFlag
    -- hs-boot summaries are EXCLUDED from extraction (item 20, 2026-08-10) —
    -- ONE site, for both variants, which is the point of this unification:
    -- a boot node shares its ModuleName with the real module, so its
    -- near-empty desugared guts would CLOBBER the real module's entry in
    -- the name-keyed 'gutsByMod' below — hiding every Core edge out of that
    -- module from 'reachableModuleClosure' and silently tiering its
    -- dependencies out of the optimized stage (observed live: the
    -- Even.hs-boot/Odd cycle baked a TypeMetadata sentinel for Odd.odd'),
    -- and merging boot guts as if they were the real module's on either
    -- path. Boot files exist for 'load''s loop-breaking only; any error in
    -- one already surfaced there, and their guts carry no bindings
    -- extraction could use.
    summaries0 <- cpSummaries plan
    let summaries = [ ms | ms <- summaries0, ms_hsc_src ms == HsSrcFile ]
    when (null summaries) $
      liftIO $ ioError (userError (pvLabel variant ++ ": empty module graph"))
    -- 'typecheck'/'core' are summed ACROSS the loop (one line each, emitted
    -- after) rather than timed per-module: the wire grammar is one line per
    -- phase per process, and a turn module always compiles alongside its
    -- preamble/stdlib dep modules in the same loop. 'core' is desugar time
    -- for EVERY module plus 'core2core' time for the modules 'cpTier' let
    -- through — one flat phase, unchanged by the tier.
    tcMsRef   <- liftIO (newIORef (0 :: Integer))
    coreMsRef <- liftIO (newIORef (0 :: Integer))
    -- Diagnostic-only accounting (NOT part of the 'tidepool-timing phase=…'
    -- wire grammar 'emitPhase' owns — see the line emitted below, distinctly
    -- prefixed 'e6-tier', deliberately outside that contract so
    -- 'ExtractTiming::parse' never has to know about it): the desugar/
    -- core2core split WITHIN the 'core' phase, plus how many modules the tier
    -- actually spared. 'core' stays one flat phase per the timing contract;
    -- this line exists purely so E6's own report can size its win against what
    -- it actually removed (core2core) rather than the whole 'core' bucket
    -- (desugar + core2core), which is a larger, unmeasured-by-this-item
    -- quantity.
    dsMsRef  <- liftIO (newIORef (0 :: Integer))
    c2cMsRef <- liftIO (newIORef (0 :: Integer))
    -- Per-module wall time (compile-attribution lane): front (typecheck +
    -- desugar) and back (core2core) halves keyed by module name and SUMMED
    -- into one entry per module via 'Map.insertWith' — tracked UNCONDITIONALLY
    -- (like 'tcMsRef'/'coreMsRef' above, cheap monotonic-clock reads), not
    -- gated on 'timing' OR on 'mSummaryT0'. Never emitted directly except
    -- through 'emitCompileSummary' (top-3, only when 'mSummaryT0' is 'Just')
    -- and 'emitModuleTiming' (every module, also gated on 'mSummaryT0') —
    -- this ref itself carries no wire contract.
    moduleMsRef <- liftIO (newIORef (Map.empty :: Map.Map String Integer))
    let targetModName  = capitalize (takeBaseName path)
        targetModName' = mkModuleName targetModName
        -- The ONE per-module front half. Re-canonicalize the module's
        -- DynFlags first (see canonicalizeDFlags): the load phase may have
        -- downgraded them for TH/QQ bytecode provisioning. NOTE: 'hscDesugar'
        -- does not consume the optLevel/unfolding-exposure flags
        -- 'canonicalizeDFlags' sets (those govern 'core2core' alone), so
        -- applying it unconditionally here costs nothing extra even for a
        -- module the tier goes on to skip.
        compileFront modSum0 = do
          let modSum = modSum0 { ms_hspp_opts = canonicalizeDFlags (ms_hspp_opts modSum0) }
          (typechecked, tcMs) <- timeSection $ do
            parsed <- parseModule modSum
            typecheckModule parsed
          liftIO (modifyIORef' tcMsRef (+ tcMs))
          hscEnv0 <- getSession
          let hscEnv   = hscUpdateFlags canonicalizeDFlags hscEnv0
              tcGblEnv = fst (tm_internals_ typechecked)
              -- Capture the inferred type of the eval's top expression NOW,
              -- before optimization can inline/rename @__user@ away. Types
              -- live on the Id in the typechecked type env; our CBOR drops
              -- them downstream (Translate.hs).
              mCapTy   = capturedUserType tcGblEnv
              -- 'cpResultBinders' is the @result@-vs-@__result@ convention:
              -- the one-shot eval wrapper (and a repl session's FIRST turn,
              -- which has no prior bindings to inject and so still runs the
              -- normal variant) can compile either; a later session turn's
              -- wrapper compiles the scaffold-reserved @__result@ only. See
              -- 'processSessionFile'.
              mResTy   = foldr (<|>) Nothing
                           [ capturedBindingType occ tcGblEnv
                           | occ <- cpResultBinders plan ]
          (desugared, dsMs) <- timeSection $ liftIO (hscDesugar hscEnv modSum tcGblEnv)
          liftIO (modifyIORef' coreMsRef (+ dsMs))
          liftIO (modifyIORef' dsMsRef (+ dsMs))
          liftIO (modifyIORef' moduleMsRef
                    (Map.insertWith (+) (moduleNameString (ms_mod_name modSum)) (tcMs + dsMs)))
          pure ModuleFront { mfSummary    = modSum
                           , mfHscEnv     = hscEnv
                           , mfTcGblEnv   = tcGblEnv
                           , mfDesugared  = desugared
                           , mfUserType   = mCapTy
                           , mfResultType = mResTy }
        -- The ONE per-module back half: the optimized-Core pass, the
        -- variant's post-compile hook (session: HPT registration of a
        -- deferred module, which is why it sees the PRE-externalize guts and
        -- the module's own typechecked env), then #313's name
        -- externalization. Returns the pre-externalize 'simplified' guts
        -- ALONGSIDE the usual post-externalize triple — a batch cycle's memo
        -- needs the former to redo 'cpAfterModule''s HPT registration on a
        -- later cycle (see 'mMemoRef' above); a lone compile (no memo) just
        -- discards it.
        compileBack mf = do
          (simplified, coreMs) <- timeSection $
            liftIO (core2core (mfHscEnv mf) (mfDesugared mf))
          liftIO (modifyIORef' coreMsRef (+ coreMs))
          liftIO (modifyIORef' c2cMsRef (+ coreMs))
          liftIO (modifyIORef' moduleMsRef
                    (Map.insertWith (+) (moduleNameString (ms_mod_name (mfSummary mf))) coreMs))
          cpAfterModule plan (mfSummary mf) (mfTcGblEnv mf) (mfHscEnv mf) simplified
          pure (simplified, (externalizeInternalTops simplified, mfUserType mf, mfResultType mf))
    -- A memo hit is trusted only if the cached entry's own 'ModSummary' has
    -- the SAME content fingerprint ('ms_hs_hash', already computed by
    -- 'depanal' for every summary — GHC's own signal for "this source
    -- changed", the same field 'invalidateModSummaryCache' resets to force a
    -- miss) as the summary THIS cycle just downswept for the same
    -- 'ModuleName'. This is required because a 'ModuleName' is not always a
    -- stable proxy for stable CONTENT: a generated module can be a pure
    -- function of the request's own effect vocabulary
    -- (@Tidepool.Effects.Core@ — content-addressed into a distinct include
    -- dir per vocabulary on the Rust side, tidepool-mcp/CLAUDE.md's
    -- "Stable-effects-core" section) while always resolving under the SAME
    -- fixed module name, so two independent requests with different
    -- vocabularies can populate/consult the shared memo under one key with
    -- genuinely different bindings (e.g. one row lacking
    -- @Tidepool.Agent.Spawn@'s @spawnSpec@) — exactly the "module names are
    -- not globally unique across independent requests" hazard design §2.2
    -- names, one step past what 'sanitizeMemo's target\/@Tidepool.Session.*@
    -- exclusion already covers (spawnrow-fix, plans/compile-daemon-design.md
    -- §7). A hash mismatch is treated as an ordinary miss: compiled fresh
    -- below, and the memo entry is overwritten with the new content via the
    -- existing 'Map.insert'. Costs nothing when content is unchanged (the
    -- overwhelmingly common case — same vocabulary, same hash, hit as
    -- before).
    -- Dependency-closure memo validity (daemon-shim-identity-fix, one step
    -- past the 'ms_hs_hash' self-check above): a module whose OWN source
    -- text is byte-identical across two independent requests
    -- (@Tidepool.Orchestrate@, which spells only the bare @M@ alias and
    -- never mentions a pinned @Finalize \<T\>@ literally) can still have a
    -- request-VARYING compiled MEANING when it imports a fixed-name/
    -- varying-content sibling (the per-window @Tidepool.Effects@ shim,
    -- co-generated into the SAME content-addressed staging dir but not
    -- co-hashed at the single-module level, tidepool-mcp/CLAUDE.md's
    -- "Stable-effects-core" section). A self-hash match is necessary but
    -- not sufficient — the compiled TcGblEnv/Core also nominally reference
    -- whatever THAT module's own home-module imports resolved to, so a memo
    -- hit is trusted only when every direct home import ALSO validated
    -- (hit, not freshly recompiled) THIS cycle. Modules are visited in
    -- dependency order ('cpSummaries' always returns a topological sort —
    -- see 'normalVariant'/'sessionVariant'), so an import's own verdict is
    -- already recorded in 'validThisCycleRef' by the time a dependent
    -- module is checked — this generalizes to any depth/shape of
    -- fixed-name/varying-content chain; no module is named here.
    let cycleModNames = Set.fromList (map ms_mod_name summaries)
        directHomeDeps modSum =
          [ mn | (_, lmn) <- ms_textual_imps modSum
               , let mn = unLoc lmn
               , mn `Set.member` cycleModNames ]
    validThisCycleRef <- liftIO (newIORef (Map.empty :: Map.Map ModuleName Bool))
    let depsValidSoFar modSum = liftIO $ do
          validMap <- readIORef validThisCycleRef
          pure (all (\d -> Map.findWithDefault False d validMap) (directHomeDeps modSum))
        recordValidity modSum isValid =
          liftIO (modifyIORef' validThisCycleRef (Map.insert (ms_mod_name modSum) isValid))
    let lookupValidMemo modSum = case mMemoRef of
          Nothing  -> pure Nothing
          Just ref -> do
            depsOk <- depsValidSoFar modSum
            if not depsOk
              then pure Nothing
              else liftIO $ do
                m <- readIORef ref
                pure $ do
                  entry <- Map.lookup (ms_mod_name modSum) m
                  if ms_hs_hash (mfSummary (gmeFront entry)) == ms_hs_hash modSum
                    then Just entry
                    else Nothing
    (fronts, results, mReachable) <- case cpTier plan of
      OptimizeEveryModule -> do
        pairs <- forM summaries $ \modSum -> do
          let mn = ms_mod_name modSum
          cached <- lookupValidMemo modSum
          case cached of
            -- Memo hit: skip parse/typecheck/desugar/core2core entirely —
            -- this is the win (§7.6: 3114ms -> 9ms per reused cycle). Still
            -- re-run 'cpAfterModule' unconditionally: it is a no-op for any
            -- module not deferred THIS cycle (the overwhelming common case —
            -- see the haddock above), and for a module that IS deferred
            -- again this cycle (the incremental-population gap this memo
            -- closes) it cheaply re-registers the already-computed iface
            -- into the HPT that 'load'' just wiped.
            Just entry -> do
              recordValidity modSum True
              cpAfterModule plan modSum (mfTcGblEnv (gmeFront entry)) (mfHscEnv (gmeFront entry)) (gmeSimplified entry)
              pure (gmeFront entry, gmeResult entry)
            Nothing -> do
              recordValidity modSum False
              mf <- compileFront modSum
              (simplified, r) <- compileBack mf
              case mMemoRef of
                Just ref -> liftIO (modifyIORef' ref (Map.insert mn (GutsMemoEntry mf simplified r)))
                Nothing  -> pure ()
              pure (mf, r)
        pure (map fst pairs, map snd pairs, Nothing)
      OptimizeCoreReachable -> do
        -- Memo consultation (resident-session addition, plans/compile-daemon-design.md
        -- §7 deviation record): with 'mMemoRef' 'Nothing' (every EXISTING
        -- caller — only 'runBatchPipeline' ever passes 'Just', and it never
        -- selects this tier), 'cached' is always 'Nothing' below, so every
        -- module takes the SAME 'compileFront'-then-'compileBack' path this
        -- branch has always taken — this generalization is behavior-
        -- preserving for every pre-existing caller BY CONSTRUCTION, not by
        -- re-review. The resident daemon is the only caller that ever
        -- passes 'Just' here.
        --
        -- A memo HIT reuses a module's cached front (needed for the
        -- reachability walk below, since it carries 'mfDesugared') without
        -- redoing parse/typecheck/desugar. A hit's cached RESULT is reused
        -- outright if the module turns out reachable this cycle — safe even
        -- though the cached entry was core2core'd (optimized) in whatever
        -- EARLIER cycle populated it: a module OUTSIDE the reachable
        -- closure contributes nothing to the wire regardless of whether its
        -- Core was ever optimized (see the un-memoized comment below this
        -- tier has always carried), so reusing a MORE-optimized cached
        -- version for a module this cycle finds non-reachable would still
        -- be safe — but it can't even arise: a NON-reachable module is
        -- never core2core'd, so the memo is never asked to serve one where
        -- reachability differs from what produced the entry.
        pairs <- forM summaries $ \modSum -> do
          cached <- lookupValidMemo modSum
          case cached of
            Just entry -> do
              recordValidity modSum True
              pure (gmeFront entry, Just (gmeResult entry))
            Nothing    -> do
              recordValidity modSum False
              mf <- compileFront modSum
              pure (mf, Nothing)
        let fs = map fst pairs
        -- Reachable-module rule (written down before implementation, per
        -- spec): a home module is REACHABLE from the target iff it IS the
        -- target, or its DESUGARED Core is transitively referenced — via a
        -- real 'Var' occurrence at any depth — from the target module's own
        -- desugared Core. Computed on desugared (pre-'core2core') Core
        -- specifically: by desugar time, typeclass/instance selection is
        -- already resolved to explicit dictionary-Var applications, so this
        -- walk would NOT miss a module imported only for an orphan instance
        -- the way a renamer/typecheck-level "used name" scan would — exactly
        -- the silent ErrorSentinel-poisoning shape 'sessionVariant's
        -- 'cpAfterModule' commentary documents. And because this codebase
        -- defines no @{-# RULES #-}@ anywhere (grep-confirmed empty),
        -- 'core2core' cannot introduce a genuinely NEW cross-module reference
        -- that wasn't already visible at this desugared stage — dictionary/
        -- instance selection is the only mechanism that could hide a
        -- reference pre-Core, and it's already resolved by here. So the
        -- desugar-stage reference graph is a sound (superset-or-equal)
        -- approximation of what the fully optimized Core actually needs.
        --
        -- A module OUTSIDE this closure would contribute zero bindings the
        -- target's Core can reach either way — Main.hs's own post-hoc,
        -- binding-level reachability walk over the final merged 'allBinds'
        -- ('Translate.reachableBinds', run from the target) would discard it
        -- regardless — so skipping 'core2core' for it changes no wire byte on
        -- a passing extraction; it only skips work whose result was always
        -- going to be thrown away.
        forceValidationOnly <- liftIO (lookupEnv "TIDEPOOL_TEST_FORCE_VALIDATION_ONLY")
        let gutsByMod = Map.fromList [ (ms_mod_name (mfSummary f), mfDesugared f) | f <- fs ]
            reachableMods0 = reachableModuleClosure targetModName' gutsByMod
            -- E6 mis-tiering fault injection (detection-power demonstration,
            -- see 00-spec.md's VERIFY section): forcibly deny a NAMED module
            -- 'core2core' regardless of whether the real closure above found
            -- it reachable — simulating exactly the mistake this tier could
            -- make. Inert unless set; never set outside a deliberate test.
            reachableMods = case forceValidationOnly of
              Just m  -> Set.delete (mkModuleName m) reachableMods0
              Nothing -> reachableMods0
        rs <- fmap concat $ forM pairs $ \(f, mCachedResult) ->
          if ms_mod_name (mfSummary f) `Set.member` reachableMods
            then case mCachedResult of
              -- Memo hit AND reachable: the cached RESULT (already
              -- core2core'd by whatever cycle inserted it) is exactly what
              -- a fresh 'compileBack' would recompute — reuse it, skipping
              -- the optimizer pass entirely.
              Just r -> pure [r]
              Nothing -> do
                (simplified, r) <- compileBack f
                case mMemoRef of
                  Just ref -> liftIO (modifyIORef' ref
                    (Map.insert (ms_mod_name (mfSummary f)) (GutsMemoEntry f simplified r)))
                  Nothing  -> pure ()
                pure [r]
            -- Not reachable: never core2core'd this cycle (matches every
            -- pre-existing caller byte for byte) and never inserted into
            -- the memo — a module a LATER cycle finds reachable must still
            -- get a real 'compileBack', never a validation-only stand-in.
            else pure []
        pure (fs, rs, Just reachableMods)
    totalTcMs   <- liftIO (readIORef tcMsRef)
    totalCoreMs <- liftIO (readIORef coreMsRef)
    liftIO (emitPhase timing "typecheck" totalTcMs)
    liftIO (emitPhase timing "core" totalCoreMs)
    -- Default-on per-compile summary (compile-attribution lane): fires
    -- whenever the caller passed 'Just' for 'mSummaryT0' (a lone compile,
    -- always; a batch cycle, never — see the haddock above), independent of
    -- 'timing' — see 'emitCompileSummary''s haddock for why. Wall time is the
    -- whole compile so far (session bootstrap through the per-module loop
    -- above), not a sum of the per-module column, since
    -- 'ghc_setup'/'ghc_load'/'inject' work outside any one module's own span.
    -- The per-module BREAKDOWN behind it stays gated on 'timing' exactly like
    -- every other detailed diagnostic in this file.
    case mSummaryT0 of
      Nothing -> pure ()
      Just _  -> do
        summaryT1 <- monotonicTime
        liftIO $ do
          moduleTimes <- readIORef moduleMsRef
          let topModules = take 3 (sortOn (negate . snd) (Map.toList moduleTimes))
          emitCompileSummary (length summaries) (elapsedMs sessionT0 summaryT1) totalTcMs totalCoreMs topModules
          emitModuleTiming timing (sortOn (negate . snd) (Map.toList moduleTimes))
    -- Diagnostic-only (see 'dsMsRef'/'c2cMsRef' haddock above): NOT part of
    -- the tidepool-timing wire grammar, so 'ExtractTiming::parse' never sees
    -- it and there is nothing to keep in sync there. Emitted only under
    -- 'OptimizeCoreReachable' (there is no tier to report otherwise), and
    -- AFTER the phase lines, exactly where it has always been.
    case mReachable of
      Just reachableMods | timing -> liftIO $ do
        totalDsMs  <- readIORef dsMsRef
        totalC2cMs <- readIORef c2cMsRef
        let allModNames = [ ms_mod_name (mfSummary f) | f <- fronts ]
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
      _ -> pure ()
    cpBeforeMerge plan loadFlag
    -- Merge: dependency module bindings first, target module last
    let isTargetMod g = moduleNameString (moduleName (mg_module g)) == targetModName
        fst3 (g, _, _) = g
        allGuts = map fst3 results
    (targetGuts, depGuts, capturedTy, resultTy) <- case filter (isTargetMod . fst3) results of
      ((tgt, ty, rty):_) -> return (tgt, [g | g <- allGuts, mg_module g /= mg_module tgt], ty, rty)
      []      -> liftIO $ ioError $ userError $
        pvLabel variant ++ ": target module '" ++ targetModName
        ++ "' not found among compiled modules: "
        ++ show (map (moduleNameString . moduleName . mg_module) allGuts)
    -- 'allTyCons' unconditionally covers EVERY compiled module, not just the
    -- tier's reachable set: TyCon/DataCon declarations are populated by the
    -- typechecker and are never touched by 'core2core' (which transforms
    -- 'mg_binds' only — the back half never re-derives 'mg_tcs'), so reading
    -- them off the DESUGARED guts costs nothing extra and keeps a
    -- validation-only module's data types available to D1's metadata walk.
    -- Order is summary order, which on the session variant (topologically
    -- sorted over the target's own import closure, so the target is last) is
    -- the dependencies-then-target order it used to build by hand.
    let allBinds  = concatMap mg_binds depGuts ++ mg_binds targetGuts
        allTyCons = concatMap (mg_tcs . mfDesugared) fronts
    hscFinal <- getSession
    warnings <- liftIO (nub . reverse <$> readIORef warnRef)
    return PipelineResult
      { prBinds  = allBinds
      , prTyCons = allTyCons
      , prHscEnv = cpFinalEnv plan hscFinal
      , prCapturedType = capturedTy
      , prResultType   = resultTy
      , prWarnings     = warnings
      }

-- | Catch any exception from a 'Ghc' action without losing the live session
-- (a bare 'IO'-level 'Control.Exception.try' cannot wrap a 'Ghc' action
-- directly). Standard 'reifyGhc'/'reflectGhc' bridge — see their haddocks in
-- @GHC.Driver.Monad@ for the canonical form this mirrors.
gTryAny :: Ghc a -> Ghc (Either SomeException a)
gTryAny act = reifyGhc (\session -> try (reflectGhc act session))

-- | Parse-only decl extraction against the ALREADY-OPEN batch session (no
-- separate 'runGhc' bootstrap — mirrors 'Tidepool.Binders.extractBindersNamed'
-- minus its own session setup, reusing its 'declItems' walker). A decl-kind
-- batch item never enters the compile loop (matches @runTurnMode@'s KDecl
-- branch — a decl turn is a name harvest, not a compile), so it costs
-- neither the 'ModIfaceCache' nor the guts memo: one 'depanal' and one parse
-- against the batch's already-booted session.
runBatchDeclItems :: FilePath -> String -> Ghc [ExportItem]
runBatchDeclItems path expectedModuleName = do
  target <- guessTarget path Nothing Nothing
  setTargets [target]
  _ <- depanal [] False
  graph <- getModuleGraph
  case filter isExpected (mgModSummaries graph) of
    (chosen : _) -> do
      pm <- parseModule chosen
      pure (concatMap declItems (hsmodDecls (unLoc (pm_parsed_source pm))))
    [] -> liftIO (ioError (userError
            ("runBatchDeclItems: no module named " ++ expectedModuleName
              ++ " in the parsed module graph")))
  where
    isExpected ms = moduleNameString (moduleName (ms_mod ms)) == expectedModuleName

-- | One batch item's compile request. A decl item never enters the compile
-- loop ('BatchDecl' — parse-only, see 'runBatchDeclItems'); a bind/expr item
-- ('BatchCompile') goes through 'runCompileCycle' with the batch's cache and
-- guts memo threaded.
data BatchItem
  = BatchDecl { biPath :: FilePath, biExpectedModule :: String }
  | BatchCompile { biPath :: FilePath, biScope :: SessionScope }

-- | One batch item's outcome, tagged by which 'BatchItem' constructor
-- produced it.
data BatchItemResult
  = BatchDeclResult [ExportItem]
  | BatchCompileResult PipelineResult

-- | Run a whole batch — ONE GHC boot, ONE 'runGhc', a live 'ModIfaceCache'
-- (§7.1) and per-module dep-guts memo (§7.6) threaded across every item, in
-- order — per plans/post-restart/batch-turns-feasibility.md §8.
--
-- @onItem index result@ is called for each item that FINISHES COMPILING, in
-- order, immediately after that item's own artifacts are ready — so a caller
-- that writes an item's output to disk from inside @onItem@ leaves every item
-- before a mid-batch failure with a complete output directory (§3's
-- run-until-first-error contract; app/Main.hs's @--turn-batch@ mode is that
-- caller). Stops at the first item — its own compile, OR its @onItem@
-- callback — that throws, returning how many items fully completed and the
-- exception that stopped the batch (if any); the caller attributes that
-- exception to the item immediately after the returned count.
runBatchPipeline
  :: [FilePath] -> [BatchItem] -> (Int -> BatchItemResult -> IO ())
  -> IO (Int, Maybe SomeException)
runBatchPipeline includes items onItem = do
  timing <- readTimingEnabled
  (libdir, startupMs) <- timeSection getLibdir
  emitPhase timing "startup" startupMs
  runGhc (Just libdir) $ do
    dflags <- getSessionDynFlags
    dflags' <- liftIO (withBuildProductsFromEnv (extractionDynFlags dflags includes))
    _ <- setSessionDynFlags dflags'
    cache   <- liftIO newIfaceCache
    memoRef <- liftIO (newIORef Map.empty)
    go cache memoRef timing (0 :: Int) items
  where
    go _ _ _ n [] = pure (n, Nothing)
    go cache memoRef timing n (item : rest) = do
      attempt <- gTryAny $ do
        result <- case item of
          BatchDecl path expected ->
            BatchDeclResult <$> runBatchDeclItems path expected
          BatchCompile path scope ->
            -- Structurally 'sessionVariant' (same deferred-module injection
            -- logic, same 'OptimizeEveryModule' tier) with its own
            -- error-message label — batch items are compiled via
            -- 'runCompileCycle' directly rather than through
            -- 'runCompile'/'runPipelineSession'.
            BatchCompileResult <$>
              runCompileCycle (Just cache) (Just memoRef) timing Nothing
                (sessionVariant scope path) { pvLabel = "runBatchPipeline" } path
        liftIO (onItem n result)
      case attempt of
        Left e   -> pure (n, Just e)
        Right () -> go cache memoRef timing (n + 1) rest

-- ---------------------------------------------------------------------------
-- Resident session (plans/compile-daemon-design.md, Phase 0): the daemon's
-- own generalization of 'runBatchPipeline''s loop — ONE 'runGhc' boot, ONE
-- 'setSessionDynFlags', serving individual compile REQUESTS one at a time
-- instead of a pre-built @[BatchItem]@ list. Transport-blind: this module
-- knows nothing about sockets or frames (Tidepool.DaemonServer owns that) —
-- it hands the caller a plain IO closure shaped exactly like
-- 'runPipelineSession', so app/Main.hs's existing dispatch can substitute it
-- in with no other change to its own call sites.
-- ---------------------------------------------------------------------------

-- | Boot ONE GHC session (session 'DynFlags' set once, from @baseIncludes@ —
-- the stdlib root), and hand the caller back a 'runPipelineSession'-shaped IO
-- closure that reuses this session's warm 'ModIfaceCache' + per-module
-- 'GutsMemo' across every call, for as long as @useCompiler@'s action runs.
--
-- Isolation (design doc §2.3): the returned closure's own target/session
-- module is compiled fresh on every call, and its guts never survive in the
-- shared memo afterward — see 'sanitizeMemo'. The shared memo therefore only
-- ever accumulates entries for the fixed, cross-request-invariant stdlib/
-- preamble tree (whatever a call's own import closure touches; warmed
-- lazily, "on first use" rather than an explicit up-front sweep — the design
-- doc's own "once at boot or on first use" allowance, §2.3).
--
-- @extraIncludes@ on each call (the request's own @--include@ dirs) apply
-- PER CYCLE, not just at boot: patched directly onto the live session's
-- 'DynFlags' via 'hscUpdateFlags' (NOT 'setSessionDynFlags', which re-runs
-- unit-state initialization — far too expensive to pay every request, and
-- exactly the cost this design exists to amortize away).
withResidentPipeline
  :: [FilePath]
  -> ((Maybe SessionScope -> FilePath -> [FilePath] -> IO PipelineResult) -> IO a)
  -> IO a
withResidentPipeline baseIncludes useCompiler = do
  timing <- readTimingEnabled
  (libdir, startupMs) <- timeSection getLibdir
  emitPhase timing "startup" startupMs
  runGhc (Just libdir) $ do
    dflags <- getSessionDynFlags
    dflags' <- liftIO (withBuildProductsFromEnv (extractionDynFlags dflags baseIncludes))
    _ <- setSessionDynFlags dflags'
    cache   <- liftIO newIfaceCache
    memoRef <- liftIO (newIORef Map.empty)
    let baseImportPaths = importPaths dflags'
    reifyGhc $ \session ->
      useCompiler $ \mscope path extraIncludes ->
        reflectGhc
          (residentCompileOne cache memoRef baseImportPaths timing mscope path extraIncludes)
          session

-- | One resident-session compile cycle, against the ALREADY-OPEN session
-- 'withResidentPipeline' booted. Patches @importPaths@ for THIS cycle only
-- (see 'withResidentPipeline'), compiles with the shared 'ModIfaceCache' +
-- 'GutsMemo', then sanitizes the memo before returning (see 'sanitizeMemo').
-- Passes 'Just' its own freshly-captured start time for @mSummaryT0@ — like a
-- lone spawn's 'runCompile', not like a batch cycle — so each daemon-served
-- request still gets the same default-on per-compile summary a direct spawn
-- would emit.
--
-- Variant: the SAME choice 'runPipelineSession' itself makes —
-- 'sessionVariant' for an active scope, 'normalVariant' otherwise — so a
-- daemon-served request always selects the identical TIER a direct spawn of
-- the same argv would have, which 'OptimizeCoreReachable'-vs-
-- 'OptimizeEveryModule' proved is NOT an interchangeable choice: the two
-- tiers can compile a validation-only (non-reachable) dependency module
-- into differently-shaped Core (raw desugared vs. fully core2core'd), and
-- 'writeClosedTargets''s metadata merge walks that Core, so swapping tiers
-- silently changed @meta.cbor@'s byte content — empirically caught by this
-- lane's own integration test before this comment was written (an earlier
-- version of this function forced 'sessionVariant' unconditionally, on the
-- mistaken assumption that a non-reachable module's Core is thrown away
-- identically either way; the metadata WALK is not, even though the final
-- wire-emitted bindings are). See 'runCompileCycle''s @OptimizeCoreReachable@
-- arm below for the memo integration that makes THIS gate — matching a
-- direct spawn's tier exactly — still get the warm-cache win for a plain,
-- non-session request: both tiers now consult @mMemoRef@, so either choice
-- here reuses the shared stdlib memo. Recorded as an implementation-forced
-- deviation from an earlier draft in plans/compile-daemon-design.md §7.
residentCompileOne
  :: ModIfaceCache -> IORef GutsMemo -> [FilePath]
  -> Bool -> Maybe SessionScope -> FilePath -> [FilePath] -> Ghc PipelineResult
residentCompileOne cache memoRef baseImportPaths timing mscope path extraIncludes = do
  sessionT0 <- monotonicTime
  hsc0 <- getSession
  setSession (hscUpdateFlags
    (\df -> df { importPaths = nub (baseImportPaths ++ extraIncludes) }) hsc0)
  let variant = case mscope of
        Just scope | isSessionScopeActive scope -> sessionVariant scope path
        _                                        -> normalVariant path
      targetModName' = mkModuleName (capitalize (takeBaseName path))
  result <- runCompileCycle (Just cache) (Just memoRef) timing (Just sessionT0) variant path
  liftIO (sanitizeMemo targetModName' memoRef)
  pure result

-- | Strip every request-scoped entry from the shared 'GutsMemo' after a
-- resident cycle: the cycle's own target module (@targetModName@) and any
-- @Tidepool.Session.*@ module ('parseSessionModule' recognizes both @Val@
-- and @Lib@ kinds — the ONE existing session-module-name recognizer, reused
-- rather than a second hand-rolled prefix check).
--
-- Deliberately NOT a literal @mMemoRef = Nothing@ for the whole cycle, which
-- a first reading of design doc §2.3's wording might suggest: that would
-- also disable READS of the already-warmed STDLIB entries the whole
-- mechanism exists to serve, defeating the daemon's purpose (every module in
-- 'cpSummaries' — stdlib deps included — would compile fresh every cycle).
-- Stripping request-scoped names post-hoc keeps the read-side win (stdlib
-- entries persist and keep accumulating across requests) while upholding the
-- actual invariant §2.2 states: a request-spanning memo must never let one
-- request's @__result@/@Val.G\<g\>@ guts reach another request's compile of
-- the same name. Recorded as an implementation-forced deviation from the
-- design doc's literal wording in plans/compile-daemon-design.md §7.
sanitizeMemo :: ModuleName -> IORef GutsMemo -> IO ()
sanitizeMemo targetModName' memoRef =
  modifyIORef' memoRef $ Map.filterWithKey $ \mn _ ->
    mn /= targetModName' && isNothing (parseSessionModule (moduleNameString mn))

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

-- | The session-setup DynFlags transform, applied once by 'runCompile' and so
-- shared by BOTH variants: 'canonicalizeDFlags' + the genericPlatform spoof +
-- exposing the @ghc@ package + clearing host SIMD. The produced 'DynFlags' is
-- byte-for-byte what the normal path always built. See the long commentary at
-- the 'runCompile' call site for the rationale of each field.
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

-- | Apply the shared, persistent build-products dir (module-granular GHC
-- recompilation avoidance across `tidepool-extract` spawns — spike-verified
-- 2026-08-20, plans/turn-latency-state-injection.md's "Direction: toward a
-- resident compile daemon" section) from @$TIDEPOOL_BUILD_PRODUCTS_DIR@, if
-- set. Points @hiDir@\/@objectDir@ at it and turns on @-fwrite-interface@ so
-- GHC's own @checkOldIface@ recompilation checking can skip an unchanged
-- home module (typically 49 of ~51 modules on a companion turn — every
-- stdlib module the compiled target doesn't itself edit) instead of
-- redoing parse\/typecheck\/desugar for it every single spawn.
--
-- A no-op (byte-identical 'DynFlags') when the env var is unset — exactly
-- like 'getLibdir''s own @$TIDEPOOL_GHC_LIBDIR@ — so every existing caller
-- that never sets it (every test suite besides the acceptance test this
-- lane adds, every non-companion eval) is untouched. The env var, not a
-- 'GhcPipeline' function parameter, is the seam deliberately: threading a
-- new parameter through 'runPipeline'\/'runPipelineSession'\/
-- 'runBatchPipeline' would ripple into every call site across
-- @app/Main.hs@ and four independent test-suites for a setting that is
-- process-wide, not per-compile — @app/Main.hs@'s own @--build-products-dir@
-- flag (the one 'tidepool-extract-cmd' surface + compile-memo allowlist
-- entry this lane adds) sets this SAME env var once at startup, before any
-- 'GhcPipeline' call.
withBuildProductsFromEnv :: DynFlags -> IO DynFlags
withBuildProductsFromEnv dflags = do
  mDir <- lookupEnv "TIDEPOOL_BUILD_PRODUCTS_DIR"
  pure $ case mDir of
    Nothing  -> dflags
    Just dir -> (`gopt_set` Opt_WriteInterface) dflags
      { hiDir = Just dir
      , objectDir = Just dir
      }

-- | The normal (non-session) variant: no injection, and E6's Core-reachability
-- tier. Everything else is 'runCompile'.
normalVariant :: FilePath -> PipelineVariant
normalVariant path = PipelineVariant
  { pvLabel = "runPipeline"
  , pvDownsweepExcludes = []
  , pvPlan = \_timing modGraphRaw -> pure CompilePlan
      { cpLoadGraph = modGraphRaw
      , cpAfterLoad = \_ -> pure ()
        -- TOPOLOGICAL RECOVERY ORDER (was: 'mgModSummaries <$> getModuleGraph',
        -- whose order is NOT guaranteed dependency-first — see its haddock).
        -- Each summary's own 'parseModule'/'typecheckModule' below redoes its
        -- typecheck INDEPENDENTLY of 'load'' (see 'compileFront'), and a
        -- genuine failure throws a spanned 'SourceError' that stops this
        -- 'forM' loop immediately — so whichever module the loop visits FIRST
        -- among a failing module and its dependents determines whether a
        -- caller sees the real diagnostic or a downstream "module X is not
        -- loaded" cascade (the dependent's own typecheck can't resolve an
        -- import that hasn't been redone yet in THIS loop, even though
        -- 'load'' already failed on it upstream). Dependency order makes this
        -- deterministic: a module with a genuine error of its own is always
        -- reached before anything that imports it, so its real error fires
        -- first and the loop never reaches the dependent at all. Same idiom
        -- 'sessionVariant' already uses one seam down, for the same reason.
      , cpSummaries = pure
          [ ms | ModuleNode _ ms <- flattenSCCs (topSortModuleGraph True modGraphRaw Nothing) ]
        -- 'runPipeline' (single-shot eval) always compiles a target named
        -- @result@; a repl session's FIRST turn also lands on this variant
        -- (no prior bindings to inject, so 'isSessionScopeActive' is still
        -- False) and its wrapper compiles @__result@, the scaffold-reserved
        -- name — see 'processSessionFile'. Try both, in that order.
      , cpResultBinders = [scaffoldOutputBase, scaffoldTargetName]
      , cpAfterModule = \_ _ _ _ -> pure ()
      , cpTier = OptimizeCoreReachable
        -- Phase barrier (backstop): a target or dependency compile error
        -- already threw a spanned 'SourceError' from inside the compile loop
        -- (each summary's own 'parseModule'/'typecheckModule' redoes its
        -- typecheck independently of 'load'', so a real user type error
        -- surfaces there with its span intact) — this MUST run AFTER the
        -- loop, not before, or that spanned diagnostic never fires and
        -- callers get this generic message instead. That is exactly why this
        -- variant fills 'cpBeforeMerge' and leaves 'cpAfterLoad' empty, while
        -- 'sessionVariant' does the opposite. The phase timings are emitted
        -- first, so a run that dies here still reports the work it did.
        -- Reaching here with 'loadFlag' still 'Failed' means the loop
        -- finished without re-surfacing whatever 'load'' choked on; stop
        -- rather than return a 'PipelineResult' built against a
        -- half-populated environment.
      , cpBeforeMerge = \loadFlag -> case loadFlag of
          Failed    -> liftIO $ ioError $ userError $
            "runPipeline: module load failed compiling " ++ path
          Succeeded -> pure ()
      , cpFinalEnv = id
      }
  }

-- | The SESSION extraction variant (active 'SessionScope' only). The same
-- 'runCompile' skeleton as 'normalVariant' — @depanal@/@load'@ then the
-- per-module parse/typecheck/desugar/@core2core@ loop over every home module,
-- returning all their guts — with the session-scope injection seam filled in:
--
--   1. The source-less @Val.G<g>@ modules are EXCLUDED from @depanal@ (no
--      source to summarise) and their thin ifaces are INJECTED into the HPT +
--      finder ('injectSessionScope', 'cpAfterLoad') so a turn module's
--      @import Val.G<g>@ resolves.
--   2. Every module that (transitively) imports one of those — the turn target
--      included — is excluded from the @load'@ graph (it cannot be compiled
--      before the Val ifaces exist) and compiled instead in the
--      post-injection loop, which also registers it back into the HPT
--      ('cpAfterModule').
--
-- Its tier is 'OptimizeEveryModule'. Compiling every home module to full -O2
-- guts (rather than extracting only the target and resolving its library
-- calls from HPT ifaces) is load-bearing — see 'cpAfterModule' below. A
-- reference turn imports @Tidepool.Prelude@ via the eval preamble; the
-- @load'@ also keeps those source deps "loaded" (GHC-58427).
sessionVariant :: SessionScope -> FilePath -> PipelineVariant
sessionVariant scope path = PipelineVariant
  { pvLabel = "runSessionPipeline"
  , pvDownsweepExcludes = excludedVal
  , pvPlan = \timing modGraphRaw -> do
      let targetModName' = mkModuleName (capitalize (takeBaseName path))
          directSummaries = [ ms | ModuleNode _ ms <- mgModSummaries' modGraphRaw ]
          importsOf ms = [ unLoc lmn | (_, lmn) <- ms_textual_imps ms ]
          -- Everything that (directly or transitively) imports an injected
          -- Val module can't go through the @load'@ below — its import can
          -- only resolve once 'cpAfterLoad''s injection has happened. This
          -- generalizes the old "just exclude the target" rule: a decl module
          -- (@Lib.G<g>@) that itself imports a Val module is ALSO a
          -- dependency needing deferral, not just the ultimate leaf target.
          -- Plain forward fixpoint over the (small, per-turn) module graph —
          -- no existing GHC utility does this specific reverse-reachability
          -- query, so this is a self-contained graph closure over data
          -- already on each 'ModSummary'.
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
          -- @LoadDependenciesOf targetHUM@, whose @createBuildPlan@ includes
          -- ALL modules reachable from the root) compiles it BEFORE the Val
          -- iface is injected, so its @import Tidepool.Session.Val.G<g>@
          -- fails → GHC error-recovery emits "Could not find module" AND
          -- inserts a FAKE empty iface into the EPS PIT for the Val module.
          -- Filtering deferred modules out makes @load'@ compile ONLY the
          -- untouched source deps; each deferred module is compiled AND its
          -- interface registered back into the HPT in the post-injection
          -- loop, in dependency order.
          depGraph = mkModuleGraph
            [ node | node <- mgModSummaries' modGraphRaw
                   , case node of
                       ModuleNode _ ms -> not (ms_mod_name ms `Set.member` deferredMods)
                       _               -> True ]
      pure CompilePlan
        -- Compile the turn's home-package SOURCE dependencies
        -- (@Tidepool.Prelude@, @Tidepool.Effects@, @Lib.G<g>@) into the HPT,
        -- but NOT the turn target itself. LoadAllTargets on depGraph (target
        -- filtered out above) — equivalent to the old @LoadDependenciesOf@
        -- but without compiling the target prematurely.
        { cpLoadGraph = depGraph
        , cpAfterLoad = \loadFlag -> do
            -- Phase barrier: unlike 'normalVariant' this variant checks
            -- EARLY — a 'Failed' dependency load stops here, before the
            -- module-graph restore, the Val iface injection, or the
            -- per-module compile ever see a half-populated HPT.
            case loadFlag of
              Failed    -> liftIO $ ioError $ userError $
                "runSessionPipeline: PHASE 1 dependency load failed compiling " ++ path
              Succeeded -> pure ()
            -- Restore the FULL module graph (target included) so the
            -- per-module typecheck can see HPT instances from dep modules:
            -- @hptSomeThingsBelowUs@ walks @moduleGraphModulesBelow
            -- (hsc_mod_graph) target@, and @load'@ left @hsc_mod_graph =
            -- depGraph@ (target absent), which would yield an empty HPT
            -- instance env ("No instance for ToJSON …").
            do hscMG <- getSession
               setSession hscMG { hsc_mod_graph = modGraphRaw }
            -- Inject the live @Val.G<g>@ ifaces into the now dep-populated
            -- HPT. AFTER @load'@, so its upsweep does not discard them; the
            -- subsequent per-module compile (no further @load'@) preserves
            -- them.
            injectT0 <- monotonicTime
            hsc0 <- getSession
            hscInjected <- injectSessionScope scope hsc0
            setSession hscInjected
            injectT1 <- monotonicTime
            -- 'inject' phase (TIDEPOOL_TIMING): the Val-iface injection
            -- alone. Session-variant-only — the normal variant never injects
            -- session Vals. FLAT, like every other phase — not summed into
            -- anything.
            liftIO (emitPhase timing "inject" (elapsedMs injectT0 injectT1))
            -- Dependency order matters now that MULTIPLE modules (not just
            -- one leaf target) may need deferred, post-injection compilation:
            -- a deferred module that itself depends on another deferred
            -- module (e.g. the target importing a Val-referencing
            -- @Lib.G<g>@) must see the latter ALREADY reinserted into the HPT
            -- by the time its own turn in the loop comes up.
            -- @mgModSummaries@/@mg_mss@ is not guaranteed topologically
            -- ordered (see its haddock); @topSortModuleGraph@ +
            -- @flattenSCCs@ (both re-exported by the umbrella 'GHC' module
            -- already imported here) give a real deps-before-dependents
            -- order.
        , cpSummaries = pure
            [ ms | ModuleNode _ ms <- flattenSCCs (topSortModuleGraph True modGraphRaw Nothing) ]
          -- Reached only once a session has a prior binding to inject
          -- ('isSessionScopeActive'); every such turn's wrapper compiles a
          -- target literally named @__result@ (scaffold-reserved, never
          -- @result@ — see 'processSessionFile').
        , cpResultBinders = [scaffoldTargetName]
          -- A deferred module (target ∪ transitive Val-importers, computed
          -- above) was deliberately excluded from the @load'@, so nothing has
          -- registered it in the HPT yet — do that here, now that the Val
          -- injection has happened, so a LATER module in the same loop that
          -- imports this one (e.g. the leaf importing a Val-referencing
          -- @Lib.G<g>@) can resolve it. Real 'ModIface'/'ModDetails' via the
          -- same tidy→iface pipeline GHC's own batch compiler uses internally
          -- ('hscTidy' wraps 'initTidyOpts'+'tidyProgram'; 'mkIfaceTc' is what
          -- 'hscSimpleIface'' uses for "a stripped down interface... where we
          -- aren't generating any object code at all" — precisely this case,
          -- since Core is extracted separately for the Cranelift JIT and
          -- nothing here ever executes via GHC's own bytecode interpreter,
          -- hence no real linkable is ever needed —
          -- 'emptyHomeModInfoLinkable' is the same legitimate "no linkable"
          -- value GHC itself uses for @.hs-boot@ modules). Mirrors
          -- 'upsweep_mod's own @addToHpt@ call.
          --
          -- This is also why the whole home graph is recompiled to full guts
          -- rather than the target alone: resolving the home-library
          -- functions it calls (@object@, @.=@, @$fToJSONInt@, @toText@, …)
          -- from their HPT interface unfoldings does NOT work — @load'@
          -- provisions those ifaces without -O2 unfoldings, so
          -- 'resolveExternals' cannot inline them, bakes a poison
          -- ErrorSentinel for each, and the masking in
          -- 'translateModuleClosed' (@trulyUnresolved@, keyed on the
          -- un-poisoned id which never appears) hides it — the sentinel then
          -- fires at run as @kind=4 TypeMetadata@. Recompiling the deps as
          -- full guts gives their bodies directly, so no library function is
          -- ever left unresolved. The target's @import Val.G<g>@ resolves
          -- from the injection above.
        , cpAfterModule = \modSum tcGblEnv hscEnv simplified ->
            when (ms_mod_name modSum `Set.member` deferredMods) $ do
              (cgGuts, modDetails) <- liftIO $ hscTidy hscEnv simplified
              iface <- liftIO $
                mkIfaceTc hscEnv Sf_None modDetails modSum (Just (cg_binds cgGuts)) tcGblEnv
              let hmi = HomeModInfo iface modDetails emptyHomeModInfoLinkable
              hscEnvNow <- getSession
              setSession (hscUpdateHPT (\hpt -> addToHpt hpt (ms_mod_name modSum) hmi) hscEnvNow)
        , cpTier = OptimizeEveryModule
          -- The load barrier already fired in 'cpAfterLoad' (see there).
        , cpBeforeMerge = \_ -> pure ()
        , cpFinalEnv = hscUpdateFlags canonicalizeDFlags
        }
  }
  where
    -- The injected source-less @Val.G<g>@ modules: excluded from the
    -- downsweep (no source to summarise) — a deferred module's @import@ of
    -- them resolves from the HPT entry 'cpAfterLoad''s injection registers.
    excludedVal = map renderSessionModule (ssValIfaces scope)
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
           , occNameString (nameOccName (idName i)) == evalUserBinder ] of
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
-- True iff @T@ (after stripping its own foralls/context) IS a function type,
-- OR MENTIONS one anywhere in its structure — a type application argument, a
-- newtype's representation, or a data constructor field, walked transitively
-- (visited-set keyed on 'TyCon', so a recursive type terminates instead of
-- looping; mirrors 'Tidepool.Translate.typeMentionsEffectMonad's walk). The
-- wider check matters because Tier0 forces the bound value to normal form
-- before tenuring: a record with a function FIELD (e.g. a companion "mounted
-- value" carrying an applied handler, PRD 21 lane C1) is not itself a
-- function type, but deep-forcing it would try to force through the
-- function field and crash — it needs the SAME store-as-is treatment a bare
-- function gets.
--
-- 'goTc' also special-cases the arrow TyCon itself. A HIGHER-KINDED field
-- instantiated at a partially-applied arrow (@data Box f = Box (f Int)@ at
-- @f = (->) Bool@) reaches 'goT' as one of @Box@'s own outer type
-- arguments — the CONCRETE @(->) Bool@, not @Box@'s abstract, unsubstituted
-- field declaration @f Int@ (which 'dataConOrigArgTys' can never resolve to
-- a function regardless of what @f@ is instantiated to, and correctly so —
-- it is genuinely opaque without that instantiation). A SATURATED arrow
-- always normalizes to GHC's own @FunTy@ sugar (an invariant GHC itself
-- maintains — see "Representation of function types" in @GHC.Core.Type@)
-- and is already caught by 'splitFunTy_maybe' above; only a PARTIAL
-- application like @(->) Bool@ survives as a bare @TyConApp@ of the
-- primitive arrow TyCon, which has neither a newtype representation nor
-- DataCons — so before this case it fell through both 'goTc' checks to
-- 'False', misclassifying the whole @Box@ value as Tier0 and crashing the
-- same deep-force this function exists to prevent.
isClosureType :: Type -> Bool
isClosureType ty0 =
  let (_, _, body) = tcSplitSigmaTy ty0
  in goT emptyUniqSet body
  where
    goT :: UniqSet TyCon -> Type -> Bool
    goT visited ty
      | Just{} <- splitFunTy_maybe ty = True
      | Just (tc, tyArgs) <- splitTyConApp_maybe ty = any (goT visited) tyArgs || goTc visited tc
      | otherwise = False

    goTc :: UniqSet TyCon -> TyCon -> Bool
    goTc visited tc
      | tc `elementOfUniqSet` visited = False
      | tyConUnique tc == fUNTyConKey || tyConUnique tc == unrestrictedFunTyConKey = True
      | otherwise =
          let visited' = addOneToUniqSet visited tc
              newtypeHit = case unwrapNewTyCon_maybe tc of
                Just (_tvs, reprTy, _coax) -> goT visited' reprTy
                Nothing -> False
              fieldHit = case tyConDataCons_maybe tc of
                Just dcs -> any (\dc -> any (\(Scaled _ ft) -> goT visited' ft)
                                             (dataConOrigArgTys dc)) dcs
                Nothing -> False
          in newtypeHit || fieldHit

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
-- See the call site in 'runCompile' ('OptimizeCoreReachable') for the full
-- soundness argument
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
  -- Config-class code must fail during compilation: a missing record field
  -- became a lazy runtime crash on 2026-08-25 instead of failing loud here.
  promoteConfigSafetyWarning Opt_WarnMissingFields $
  promoteConfigSafetyWarning Opt_WarnIncompletePatterns $
  promoteConfigSafetyWarning Opt_WarnIncompleteUniPatterns $
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
  -- splices run host-agnostically.
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

promoteConfigSafetyWarning :: WarningFlag -> DynFlags -> DynFlags
promoteConfigSafetyWarning warning =
  (`wopt_set_fatal` warning) . (`wopt_set` warning)

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
-- defining module, with a STABLE disambiguator baked into the OccName
-- (@k@ → @Probe.k_t3@, where @3@ is @k@'s ordinal position among this
-- module's own top-level binders), so @Translate.stableVarId@ yields a
-- globally unique, deterministic VarId. The ordinal — not the binder's raw
-- GHC 'Unique' — is what makes this deterministic ACROSS separate compiles
-- of the same source: 'mg_binds'\'s order is a pure function of this
-- module's own source and simplifier passes, never of how many Uniques the
-- surrounding GHC session happened to consume before reaching this module
-- (which a warm build-products-dir compile perturbs — see
-- 'Tidepool.Translate.stabilizeLocalUniques'\'s doc, the companion fix for
-- NESTED binders; this function only ever rewrites TOP-LEVEL ones, and
-- together the two close plans/turn-latency-state-injection.md's
-- build-products-dir determinism gap). Internal names cannot be referenced
-- from other modules' ModGuts, so substituting binder + occurrences within
-- the module is complete. Nested binders are untouched: their uniques cannot
-- collide with top-level uniques of the same module, and cross-module nested
-- references are lexically impossible.
externalizeInternalTops :: ModGuts -> ModGuts
externalizeInternalTops guts = guts { mg_binds = map goTop (mg_binds guts) }
  where
    m = mg_module guts
    topBinders = concatMap binders (mg_binds guts)
      where binders (NonRec b _) = [b]
            binders (Rec ps)     = map fst ps
    fixes = mkVarEnv [ (v, externalize ordinal v)
                     | (ordinal, v) <- zip [0 :: Int ..] topBinders
                     , not (isExternalName (idName v)) ]
    externalize ordinal v =
      let n    = idName v
          u    = nameUnique n
          occ  = nameOccName n
          occ' = mkOccName (occNameSpace occ)
                           (occNameString occ ++ "_t" ++ show ordinal)
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
