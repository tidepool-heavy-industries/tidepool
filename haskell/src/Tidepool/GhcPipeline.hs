module Tidepool.GhcPipeline
  ( runPipeline, runPipelineSession, PipelineResult(..), dumpCore ) where

import GHC
import GHC.Driver.Main (hscDesugar, batchMsg)
import GHC.Driver.Env (hscUpdateFlags, hsc_home_unit)
import GHC.Driver.Make (load', summariseFile)
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
import GHC.Utils.Outputable (renderWithContext, defaultSDocContext, ppr)
import GHC.Types.Id (idName, idType)
import GHC.Types.TypeEnv (typeEnvIds)
import GHC.Tc.Types (TcGblEnv, tcg_type_env)
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
import Tidepool.Session
  ( SessionScope, isSessionScopeActive, injectSessionScope )

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
  -- but cross-turn typechecking of references (round-3 plan) may need a
  -- structured @IfaceType@ instead of this string. See plans/ghci-swarm-orchestration.md §0.3.
  , prCapturedType :: Maybe String
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
-- @Val.G<g>@ ifaces into the HPT, then compile the (reference) target via
-- 'summariseFile' + 'hscDesugar' + 'core2core' — the normal MODULE pipeline,
-- but bypassing downsweep, which would reject the source-less session modules
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
    let dflags' = extractionDynFlags dflags includes
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
    -- unpoison: keep the EPS healthy under the TH/QQ downgrade by unsetting
    -- Opt_IgnoreInterfacePragmas on every summary (see the depanal/load'
    -- haddock above). The bytecode-vs-object provisioning choice is made
    -- session-wide in canonicalizeDFlags (Opt_UseBytecodeRatherThanObjects) —
    -- it has to be set before downsweep, since 'load' re-derives each module's
    -- backend and ignores a field patched onto a summary here.
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
      -- Capture the inferred type of the eval's top expression NOW, before
      -- optimization can inline/rename @__user@ away. Types live on the Id in
      -- the typechecked type env; our CBOR drops them downstream (Translate.hs).
      let mCapturedTy = capturedUserType tcGblEnv
      desugared <- liftIO $ hscDesugar hscEnv modSum tcGblEnv
      simplified <- liftIO $ core2core hscEnv desugared
      return (externalizeInternalTops simplified, mCapturedTy)
    -- Merge: dependency module bindings first, target module last
    let targetModName = capitalize (takeBaseName path)
        isTargetMod g = moduleNameString (moduleName (mg_module g)) == targetModName
        allGuts = map fst results
    (targetGuts, depGuts, capturedTy) <- case filter (isTargetMod . fst) results of
      ((tgt, ty):_) -> return (tgt, [g | g <- allGuts, mg_module g /= mg_module tgt], ty)
      []      -> liftIO $ ioError $ userError $
        "Target module '" ++ targetModName ++ "' not found among compiled modules: "
        ++ show (map (moduleNameString . moduleName . mg_module) allGuts)
    let allBinds = concatMap mg_binds depGuts ++ mg_binds targetGuts
        allTyCons = concatMap mg_tcs depGuts ++ mg_tcs targetGuts
    hscEnv <- getSession
    return PipelineResult
      { prBinds  = allBinds
      , prTyCons = allTyCons
      , prHscEnv = hscEnv
      , prCapturedType = capturedTy
      }

capitalize :: String -> String
capitalize [] = []
capitalize (c:cs) = toUpper c : cs

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

-- | The SESSION extraction path (active 'SessionScope' only). Inject the live
-- @Val.G<g>@ ifaces into the HPT, then compile the reference target through the
-- normal module pipeline ('summariseFile' → 'typecheckModule' → 'hscDesugar' →
-- 'core2core'), bypassing downsweep (which rejects source-less session modules).
-- Single-module: the wrapped turn imports the session/lib modules + base, so its
-- own ModGuts hold the JIT-able Core. Returns the same 'PipelineResult' shape as
-- the normal path so all downstream translation is identical.
runSessionPipeline :: SessionScope -> FilePath -> [FilePath] -> IO PipelineResult
runSessionPipeline scope path includes = do
  libdir <- getLibdir
  runGhc (Just libdir) $ do
    dflags <- getSessionDynFlags
    setSessionDynFlags (extractionDynFlags dflags includes)
    -- Inject the live session ifaces (readIface raw → typecheckIface → HPT +
    -- source-less finder entry). NOTE: the gate already guaranteed the scope is
    -- active; this is where the Option-C type plane enters the compile.
    hsc0 <- getSession
    hscInjected <- injectSessionScope scope hsc0
    setSession hscInjected
    hsc <- getSession
    let hscEnv = hscUpdateFlags canonicalizeDFlags hsc
        homeU  = hsc_home_unit hscEnv
    esum <- liftIO $ summariseFile hscEnv homeU mempty path Nothing Nothing
    modSum0 <- case esum of
      Left _   -> liftIO $ ioError $ userError
        ("runPipelineSession: summariseFile failed for " ++ path)
      Right ms -> pure ms
    let modSum = modSum0 { ms_hspp_opts = canonicalizeDFlags (ms_hspp_opts modSum0) }
    parsed      <- parseModule modSum
    typechecked <- typecheckModule parsed
    let tcGblEnv    = fst (tm_internals_ typechecked)
        mCapturedTy = capturedUserType tcGblEnv
    desugared  <- liftIO $ hscDesugar hscEnv modSum tcGblEnv
    simplified <- liftIO $ core2core hscEnv desugared
    let guts = externalizeInternalTops simplified
    hscFinal <- getSession
    return PipelineResult
      { prBinds        = mg_binds guts
      , prTyCons       = mg_tcs guts
      , prHscEnv       = hscUpdateFlags canonicalizeDFlags hscFinal
      , prCapturedType = mCapturedTy
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
  (`gopt_unset` Opt_ShowValidHoleFits) $
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
