module Tidepool.GhcPipeline (runPipeline, PipelineResult(..), dumpCore) where

import GHC
import GHC.Driver.Main (hscDesugar, batchMsg)
import GHC.Driver.Env (hscUpdateFlags)
import GHC.Driver.Make (load')
import GHC.Types.Error (mkUnknownDiagnostic)
import GHC.Unit.Module.Graph (mapMG)
import GHC.Core.Opt.Pipeline (core2core)
import GHC.Core.Ppr (pprCoreBindings)
import GHC.Driver.Session
  ( updOptLevel, gopt_set, gopt_unset
  , packageFlags, PackageFlag(..), PackageArg(..), ModRenaming(..) )
import GHC.Unit.Module.ModGuts (ModGuts(..))
import GHC.Core (CoreBind, Bind(..), Expr(..), Alt(..))
import GHC.Platform (genericPlatform)
import GHC.Utils.Outputable (renderWithContext, defaultSDocContext)
import GHC.Types.Id (idName)
import GHC.Types.Name (nameOccName, nameUnique, mkExternalName)
import GHC.Types.Name.Occurrence (mkOccName, occNameSpace, occNameString)
import GHC.Types.Var (setVarName)
import GHC.Types.Var.Env (mkVarEnv, lookupVarEnv)
import GHC.Types.Unique (getKey)
import Data.Maybe (fromMaybe)
import System.Process (readProcess)
import System.Environment (lookupEnv)
import System.FilePath (takeBaseName)
import Control.Monad.IO.Class (liftIO)
import Control.Monad (forM, when)
import Data.Char (toUpper)

data PipelineResult = PipelineResult
  { prBinds  :: [CoreBind]
  , prTyCons :: [TyCon]
  , prHscEnv :: HscEnv
  }

runPipeline :: FilePath -> [FilePath] -> IO PipelineResult
runPipeline path includes = do
  libdir <- getLibdir
  runGhc (Just libdir) $ do
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
    let dflags' = canonicalizeDFlags dflags
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
    setSessionDynFlags dflags'
    target <- guessTarget path Nothing Nothing
    setTargets [target]
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
    let unpoison ms =
          ms { ms_hspp_opts = gopt_unset (ms_hspp_opts ms) Opt_IgnoreInterfacePragmas }
    _ <- load' Nothing LoadAllTargets mkUnknownDiagnostic (Just batchMsg)
               (mapMG unpoison modGraphRaw)
    modGraph <- getModuleGraph
    let summaries = mgModSummaries modGraph
    when (null summaries) $
      liftIO $ ioError (userError "runPipeline: empty module graph")
    -- Process all modules: parse, typecheck, desugar, optimize each.
    -- Re-canonicalize each module's DynFlags first (see canonicalizeDFlags):
    -- the load phase may have downgraded them for TH/QQ bytecode provisioning.
    results <- forM summaries $ \modSum0 -> do
      let modSum = modSum0 { ms_hspp_opts = canonicalizeDFlags (ms_hspp_opts modSum0) }
      parsed <- parseModule modSum
      typechecked <- typecheckModule parsed
      hscEnv0 <- getSession
      let hscEnv = hscUpdateFlags canonicalizeDFlags hscEnv0
      let tcGblEnv = fst (tm_internals_ typechecked)
      desugared <- liftIO $ hscDesugar hscEnv modSum tcGblEnv
      simplified <- liftIO $ core2core hscEnv desugared
      return (externalizeInternalTops simplified)
    -- Merge: dependency module bindings first, target module last
    let targetModName = capitalize (takeBaseName path)
        isTargetMod g = moduleNameString (moduleName (mg_module g)) == targetModName
    (targetGuts, depGuts) <- case filter isTargetMod results of
      (tgt:_) -> return (tgt, [g | g <- results, mg_module g /= mg_module tgt])
      []      -> liftIO $ ioError $ userError $
        "Target module '" ++ targetModName ++ "' not found among compiled modules: "
        ++ show (map (moduleNameString . moduleName . mg_module) results)
    let allBinds = concatMap mg_binds depGuts ++ mg_binds targetGuts
        allTyCons = concatMap mg_tcs depGuts ++ mg_tcs targetGuts
    hscEnv <- getSession
    return PipelineResult
      { prBinds  = allBinds
      , prTyCons = allTyCons
      , prHscEnv = hscEnv
      }

capitalize :: String -> String
capitalize [] = []
capitalize (c:cs) = toUpper c : cs

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
-- "Unsupported primop: clz#". See plans/qq-spike.md (repro matrix M1-M8 in
-- scratch/qq-spike/).
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
canonicalizeDFlags :: DynFlags -> DynFlags
canonicalizeDFlags dflags =
  gopt_set (gopt_set (gopt_unset (gopt_unset (updOptLevel 2 $ dflags
        { backend = noBackend
        , ghcLink = NoLink
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
