{-# LANGUAGE RankNTypes #-}

module Main where

import System.Environment (getArgs)
import System.FilePath (takeBaseName, takeDirectory, takeFileName, (</>))
import System.Directory (createDirectoryIfMissing, setCurrentDirectory)
import qualified Data.ByteString as BS
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import qualified Data.Sequence as Seq
import Control.Exception
  ( evaluate, try, throwIO, SomeAsyncException, SomeException, Exception
  , fromException, toException )
import Data.List (isPrefixOf, intercalate)
import Data.Maybe (fromMaybe, mapMaybe, isJust)
import Data.Word (Word64)
import Control.Monad (foldM, forM, forM_, void)
import System.Exit (ExitCode(..), exitWith)
import System.IO (hPutStrLn, stderr, stdin, stdout, hSetBinaryMode, hSetEncoding, utf8)
import qualified System.Info as SystemInfo

import GHC.Types.SourceError (SourceError)
import GHC (Module, ModuleName, moduleName, moduleNameString, moduleUnit)
import GHC.Driver.Env (HscEnv)
import GHC.Unit.Types (unitString)
import GHC.Core (Bind(..), CoreBind)
import GHC.Core.DataCon (DataCon)
import GHC.Types.Name (nameOccName, nameModule_maybe)
import GHC.Types.Id (idName)
import GHC.Types.Name.Occurrence (occNameString)
import qualified Data.Text as T

import Tidepool.Binders
  ( extractBindersNamed
  , extractStmtBinders, classifyBlock, exportItemName
  , analyzeCell, renderCellCheckSource, CellSplitError(..), CellSourceSpan(..)
  , CellSourcePlan(..), installCellDisplayDeclarations
  , declarationSourceWithTemplate, renderDeclarationForTemplate
  , TurnKind(..), parseTurnKind
  , TemplateSelector(..), templateSelectorForVerdict, templateSelectorWireName
  , StmtBinders(..), TurnOut(..), renderVerdictsJson )
import Tidepool.Artifacts
  ( cborFileName, pruneAllClosedArtifacts, writeClosedTargets
  , writeWholeModuleClosed, runMultiTargetClosed, renderAsksJson )
import Tidepool.GhcPipeline
  ( PipelineSelection(..), PreparedPipelineResult(..)
  , runPipelineSessionSelected, CompilePurpose(..), PipelineResult(..), dumpCore
  , withResidentPipelineSelectedRequests, CellDisplayPass(..), cellDisplayDeclarations, checkCellInstances
  , registerResidentEvictionHook )
import Tidepool.ExecutionEncode (encodeWireProgram)
import Tidepool.ExecutionProjection (ProjectionContext(..), ProjectionError(..), projectPreparedTargetWithConstructors, resolveTextPackageUnit)
import Tidepool.PreparedFormatting (resolveFormattingAuthority)
import Tidepool.ExecutionSchema
  ( Architecture(..), Endianness(..), SymbolIdentity(..), TargetDescriptor(..) )
import Tidepool.PreparedStg
  ( PreparedModule(..), PreparedBodyCache, newPreparedBodyCache
  , evictPreparedBodyMatching )
import Tidepool.PreparedRecovery
  ( RecoveryFailure, RecoveredClosure(..), recoverPreparedClosure )
import qualified Tidepool.WorkerServer as WorkerServer
import Tidepool.DiagJson
  ( ReportOutcome(..), DiagSeverity(..), Diag(..), SourceRejection(..)
  , diagsFromSourceError, diagFromException, renderDiagsJson )
import Tidepool.ExtractUtil (capitalize)
import Tidepool.ExtractRequest (InspectionRequest(..), WorkerRequest(..), workerRequestFromArgv)
import Tidepool.Introspection (InspectionResult(..), encodeInspectionResults, runInspection)
import Tidepool.Session
  ( SessionScope(..), scaffoldTargetName, preparedScaffoldTargetName, preparedResumeTargetName
  , preparedDecodeTargetName, preparedApplyEntryTargetName, preparedApplyValueTargetName
  , scaffoldOutputBase, parseSessionModule )
import Tidepool.FatIface
  ( FatIfaceCache, newFatIfaceCache, evictFatIfaceMatching
  , OwnerInterfaceCache, newOwnerInterfaceCache, evictOwnerInterfaceMatching )
import Tidepool.SessionArtifacts
  ( mkBoundBinders, parseValModule )
import Tidepool.Translate
  ( ClosedModule(..), UnresolvedVar(..), collectDataCons
  , collectTransitiveDCons, collectUsedDataCons, mergeMetaPreserving
  , targetBindingHasIO, translateBinds, translateModuleClosed
  , wiredInDataCons )
import Tidepool.CborEncode (encodeTree, encodeMetadata, encodeTurnOut, encodeCellOut)
import Tidepool.Timing (readTimingEnabled, timePhase)
import Tidepool.TurnSource (extractModuleName, spliceTemplate)

-- | The retained-generation 'Set.Set' threads a request's
-- @--retained-generation@ symbols (see 'Tidepool.RetainedUnfoldings') into
-- the compile call so an imported retained-generation symbol's unfolding is
-- withheld from GHC's own simplifier rather than being inlined into a
-- consumer compiled in the same session. Every call site outside
-- 'processFile''s 'PreparedStg' compile passes 'Set.empty' (a true no-op):
-- only a prepared-STG compile ever recovers/persists a retained-generation
-- 'GlobalDecl' reference, so 'LegacyCore' modes (inspection/turn/cell) have
-- nothing to withhold. The resident-daemon path ('withResidentPipelineSelectedRequests',
-- used only behind @--worker-loop-v1@) honors this parameter per request too,
-- via a single installed plugin that reads a per-request 'IORef' cell.
type Compiler =
  forall result. PipelineSelection result
  -> Set.Set SymbolIdentity
  -> CompilePurpose
  -> Maybe SessionScope
  -> FilePath
  -> [FilePath]
  -> Maybe FilePath
  -> IO result

-- | Prepared-recovery caches ('recoverPreparedClosure'), threaded alongside
-- 'Compiler' into every call site that can reach 'prepareArtifacts'. In the
-- resident daemon ('main''s @--worker-loop-v1@ branch) these are created
-- ONCE, before 'withResidentPipelineSelectedRequests' boots its session, and an
-- eviction hook registered via 'registerResidentEvictionHook' drops every
-- target module compiled by a request and any @Tidepool.Session.*@ module from both
-- caches at the same request boundary 'sanitizeMemo' cleans the compile
-- memo -- library modules stay warm for the daemon's lifetime (it restarts on
-- a toolchain stamp change). The one-shot (non-daemon) path instead builds a
-- fresh 'RecoveryCaches' per invocation via 'freshRecoveryCaches', since
-- 'main' runs that branch exactly once per process anyway.
data RecoveryCaches = RecoveryCaches
  { rcFatIface :: FatIfaceCache
  , rcOwnerIface :: OwnerInterfaceCache
  , rcPreparedBodies :: PreparedBodyCache
  }

freshRecoveryCaches :: IO RecoveryCaches
freshRecoveryCaches = RecoveryCaches
  <$> newFatIfaceCache <*> newOwnerInterfaceCache <*> newPreparedBodyCache

-- | True for a 'Module' whose cached recovery state must not survive past
-- this request: the request's own target module, or any
-- @Tidepool.Session.*@ module (both mirror 'Tidepool.GhcPipeline.sanitizeMemo''s
-- own predicate for 'GutsMemo', over the same 'ModuleName').
staleRecoveryModule :: ModuleName -> Module -> Bool
staleRecoveryModule targetModName' owner =
  moduleName owner == targetModName'
    || isJust (parseSessionModule (moduleNameString (moduleName owner)))

-- | Install the daemon-lifetime 'RecoveryCaches'' eviction into
-- 'Tidepool.GhcPipeline''s resident request boundary. Call exactly once,
-- before entering 'withResidentPipelineSelectedRequests'.
registerRecoveryCacheEviction :: RecoveryCaches -> IO ()
registerRecoveryCacheEviction caches =
  registerResidentEvictionHook $ \targetModName' -> do
    evictFatIfaceMatching (rcFatIface caches) (staleRecoveryModule targetModName')
    evictOwnerInterfaceMatching (rcOwnerIface caches) (staleRecoveryModule targetModName')
    evictPreparedBodyMatching (rcPreparedBodies caches) (staleRecoveryModule targetModName')

data LocatedCellRejection = LocatedCellRejection CellSourceSpan String
  deriving Show
instance Exception LocatedCellRejection

throwCellSplitError :: CellSplitError -> IO a
throwCellSplitError errorValue = case errorValue of
  CellPrologueFailure sourceSpan message ->
    throwIO (LocatedCellRejection sourceSpan message)
  CellLexFailure ->
    throwIO (LocatedCellRejection (CellSourceSpan 1 1 1 1)
      "GHC could not lex the notebook cell")
  CellHeaderFailure message -> fail ("cell check template header: " ++ message)

-- | Serve one typed request. Stdout contains exactly one diagnostics document;
-- stderr is the human-readable channel.
main :: IO ()
main = do
  rawWorkerRequest <- getArgs
  if rawWorkerRequest == ["--worker-loop-v1"]
    then do
      hSetBinaryMode stdin True
      hSetBinaryMode stdout True
      -- Daemon-lifetime recovery caches: created ONCE per daemon process,
      -- before the resident session boots, and evicted at each request
      -- boundary by the hook registered here (see 'RecoveryCaches').
      caches <- freshRecoveryCaches
      registerRecoveryCacheEviction caches
      withResidentPipelineSelectedRequests [] $ \runRequest ->
        WorkerServer.runWorkerLoop
          (\cwd argv ->
            runRequest $ \compiler ->
              setCurrentDirectory cwd >> runWorkerInvocation compiler caches argv)
    else do
      hSetEncoding stdout utf8
      -- One-shot path: 'main' runs this branch exactly once per process, so
      -- a cache created here is already "fresh per invocation".
      caches <- freshRecoveryCaches
      runWorkerInvocation runPipelineSessionSelected caches rawWorkerRequest >>= exitWith

-- | Decode a Rust worker request and run one compilation. Direct and daemon transports use
-- the same versioned payload and therefore the same dispatch path.
runWorkerInvocation
  :: Compiler -> RecoveryCaches -> [String] -> IO ExitCode
runWorkerInvocation compiler caches rawWorkerRequest = do
  parsedWorkerRequest <- case workerRequestFromArgv rawWorkerRequest of
    Left err -> hPutStrLn stderr err >> pure Nothing
    Right (Just request) -> pure (Just request)
    Right Nothing -> hPutStrLn stderr "worker requires a versioned request" >> pure Nothing
  case parsedWorkerRequest of
    Nothing -> pure (ExitFailure 2)
    Just request -> runParsedInvocation compiler caches request

runParsedInvocation
  :: Compiler -> RecoveryCaches -> WorkerRequest -> IO ExitCode
runParsedInvocation compiler caches parsedWorkerRequest = do
  -- Read once per invocation (see Tidepool.Timing) and thread down;
  -- TIDEPOOL_TIMING is diagnostic-only and never touches stdout/the emitted
  -- files — see the module doc there and tidepool-harness/src/timing.rs.
  timing <- readTimingEnabled
  -- Apply the harness language profile by rewriting a scratch copy:
  -- rewrite the target to a pragma-prepended scratch copy BEFORE any mode
  -- dispatch below, so every mode (one-shot, session, turn) sees a plain
  -- file with no pragma-block requirement of its own. See
  -- 'spliceHarnessProfilePragma'.
  args <- if requestHarnessProfile parsedWorkerRequest
            then spliceHarnessProfilePragma parsedWorkerRequest
            else pure parsedWorkerRequest
  dispatch compiler caches timing args

-- | Dispatch one decoded worker request.
dispatch
  :: Compiler -> RecoveryCaches -> Bool -> WorkerRequest -> IO ExitCode
dispatch compiler caches timing args =
  case requestFiles args of
    [] -> reportDiags (Left (toException (userError "worker request contains no input")))
    (file : _)
        -- Classification consumes every input; all other modes use the first.
        | requestCell args                        -> runCellMode compiler args file
        | requestClassify args                    -> runClassifyMode timing args
        | not (null (requestInspections args))    -> runInspectionMode compiler args file
        -- A turn may also carry session fields, so it precedes session dispatch.
        | requestTurn args                        -> runTurnMode compiler caches args file
        -- Multi-target compilation may also carry a stable-value scope.
        | not (null (requestTargets args))        -> timePhase timing "total" (processFile compiler caches timing args file)
        -- Normal one-shot extraction.
        | otherwise                           -> timePhase timing "total" (processFile compiler caches timing args file)

runInspectionMode :: Compiler -> WorkerRequest -> FilePath -> IO ExitCode
runInspectionMode compiler args _path = do
  res <- trySynchronous $ do
    let queries = requestInspections args
    out <- maybe (fail "inspection request is missing its output path") pure (requestInspectOut args)
    let scope = if hasSessionScope args then Just (scopeFromWorkerRequest args) else Nothing
    if length queries /= length (requestFiles args)
      then fail "inspection request must carry exactly one source per query"
      else pure ()
    results <- fmap concat $ forM (zip (requestFiles args) queries) $ \(path, query) -> do
      let purpose = case query of
            InspectTypeSearch _ -> LookupTypeCompile
            _ -> GeneralCompile
      compiled <- try (compiler LegacyCore Set.empty purpose scope path (requestIncludes args) (requestBuildProductsDir args))
      case compiled of
        Left exception -> case fromException exception of
          Just (sourceError :: SourceError) ->
            pure [InspectionRejected (renderInspectionDiagnostics sourceError)]
          Nothing -> throwIO exception
        Right successful -> runInspection
          (prHscEnv successful)
          (prTargetTcGblEnv successful)
          (prTargetRdrEnv successful)
          (prCapturedTypes successful)
          [query]
    BS.writeFile out (encodeInspectionResults results)
  reportDiags res

renderInspectionDiagnostics :: SourceError -> String
renderInspectionDiagnostics = intercalate "\n" . map render . diagsFromSourceError
  where
    render diagnostic = location diagnostic ++ dMessage diagnostic
    location diagnostic = case dFile diagnostic of
      Just (file, line, column, _, _) -> file ++ ":" ++ show line ++ ":" ++ show column ++ ": "
      Nothing -> ""


-- | Prepend the harness language profile to a scratch copy of the first input.
-- Putting the profile in source keeps it visible to GHC downsweep and to
-- source-based cache keys. The caller's file is never modified; diagnostics
-- are shifted by the inserted line.
spliceHarnessProfilePragma :: WorkerRequest -> IO WorkerRequest
spliceHarnessProfilePragma args = case requestFiles args of
  [] -> pure args
  (file : rest) -> do
    src <- readFile file
    let outDir = fromMaybe (takeDirectory file </> takeBaseName file ++ "_cbor") (requestOutDir args)
        scratchPath = outDir </> takeFileName file
    createDirectoryIfMissing True outDir
    writeFile scratchPath (harnessProfilePragmaLine ++ "\n" ++ src)
    pure args { requestFiles = scratchPath : rest }

-- | Harness language extensions. A cross-language consistency test pins this
-- to the runtime-owned canonical eval dialect.
harnessProfilePragmaLine :: String
harnessProfilePragmaLine =
  "{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, FlexibleInstances, UndecidableInstances, GADTs, KindSignatures, RankNTypes, PartialTypeSignatures, ScopedTypeVariables, ExtendedDefaultRules, LambdaCase, TupleSections, MultiWayIf, RecordWildCards, NamedFieldPuns, ViewPatterns, BangPatterns, TypeApplications, BlockArguments, NumericUnderscores, MultilineStrings, DeriveFunctor, DeriveFoldable, DeriveTraversable, DeriveGeneric, DeriveAnyClass, StandaloneDeriving, QuasiQuotes, DuplicateRecordFields, OverloadedRecordDot, OverloadedLabels #-}"

-- | The shared epilogue every dispatch arm ends on: render the fixed-shape
-- JSON diagnostics report to stdout from a captured extraction result, with a
-- human-readable debug copy on stderr, exiting non-zero on failure. Also used
-- in parse-only modes (e.g. 'runClassifyMode') where no live GHC session
-- exists to ever throw a 'SourceError' — 'fromException' can only take the
-- 'Nothing' branch there.
reportDiags :: Either SomeException () -> IO ExitCode
reportDiags (Left e) = do
  let (outcome, diags) = case fromException e of
        Just (se :: SourceError) -> (ReportSourceFailure, diagsFromSourceError se)
        Nothing -> case fromException e of
          Just (SourceRejection message) ->
            (ReportSourceFailure, [Diag Nothing DiagError message])
          Nothing -> case fromException e of
            Just (LocatedCellRejection (CellSourceSpan sl sc el ec) message) ->
              (ReportSourceFailure,
                [Diag (Just ("<cell>", sl, sc, el, ec)) DiagError message])
            Nothing -> (ReportWorkerFailure, [diagFromException e])
  putStrLn (renderDiagsJson outcome diags)
  -- Debug copy for humans only; stdout (above) is the authoritative machine
  -- contract.
  case fromException e of
    Just (se :: SourceError) -> hPutStrLn stderr ("Compilation failed.\n" ++ show se)
    Nothing -> hPutStrLn stderr $ "Error: " ++ show e
  pure (ExitFailure 1)
reportDiags (Right ()) = putStrLn (renderDiagsJson ReportSuccess []) >> pure ExitSuccess

-- | Whether a generic extraction needs stable session values in scope.
hasSessionScope :: WorkerRequest -> Bool
hasSessionScope args = not (null (requestInjectVals args)) || isJust (requestSessionRoot args)

-- | Project the session portion of a worker request. Callers decide whether
-- the resulting scope is active.
scopeFromWorkerRequest :: WorkerRequest -> SessionScope
scopeFromWorkerRequest args = SessionScope
  { ssRoot      = fromMaybe "" (requestSessionRoot args)
  , ssValIfaces = mapMaybe parseValModule (requestInjectVals args)
  }

processFile
  :: Compiler -> RecoveryCaches -> Bool -> WorkerRequest -> FilePath -> IO ExitCode
processFile compiler caches timing args path = do
  let mOutDir = requestOutDir args
      mTarget = requestTarget args
  hPutStrLn stderr $ "Processing: " ++ path
  res <- trySynchronous $ do
    -- Multi-target extraction can inject stable session values without
    -- becoming a session bind/reference operation.
    let scope = if hasSessionScope args then Just (scopeFromWorkerRequest args) else Nothing
    prepared <- compiler PreparedStg (Map.keysSet (requestRetainedGenerations args)) GeneralCompile scope path (requestIncludes args) (requestBuildProductsDir args)
    let result = pprPipelineResult prepared
    let binds = prBinds result
        tycons = prTyCons result
        hscEnv = prHscEnv result
        -- Inferred type of the eval's top expression (the @__user@ binding),
        -- threaded into meta.cbor for the Rust side. Nothing for non-eval
        -- extractions (no @__user@). See GhcPipeline.capturedUserType.
        mCapturedTy = fmap T.pack (prCapturedType result)
        -- Success-path GHC warnings for the target module (empty on a clean
        -- compile). See GhcPipeline.prWarnings.
        warnTexts = map T.pack (prWarnings result)
    hPutStrLn stderr $ "  Top-level bindings: " ++ show (length binds)

    if requestDumpCore args
      then hPutStrLn stderr (dumpCore binds)
      else return ()

    let outDir = case mOutDir of
          Just dir -> dir
          Nothing  -> takeDirectory path </> takeBaseName path ++ "_cbor"
    createDirectoryIfMissing True outDir

    let preparedTargets = case requestTargets args of
          targets@(_ : _) -> targets
          [] -> maybe [] pure mTarget
    preparedArtifacts <- prepareArtifacts caches path hscEnv (pprModules prepared) preparedTargets
      (standardAuxiliaryRoots binds) (requestRetainedGenerations args)
    let preparedConstructors = concatMap paConstructors preparedArtifacts

    if not (null (requestTargets args))
      -- Explicit multi-target mode (--targets a,b): takes priority over
      -- --target/--all-closed, which stay untouched below for every other
      -- caller. One runPipeline invocation (already run, above), several
      -- named targets, one merged meta.cbor — see 'runMultiTargetClosed'.
      then runMultiTargetClosed timing outDir hscEnv binds tycons preparedConstructors mCapturedTy warnTexts (requestTargets args)
      else case (mTarget, requestAllClosed args) of
      (_, True) -> do
        -- All-closed mode: translate each binding independently via translateModuleClosed
        -- Use original names (not deduped) since translateModuleClosed looks up by name.
        -- Skip duplicates (GHC may produce multiple bindings with the same name).
        -- Include all top-level binders, not just External ones.
        -- GHC may mark user-defined bindings as Internal after optimization.
        -- Filter out GHC-generated names (starting with '$').
        -- Errors from translateModuleClosed are caught and those bindings are skipped.
        -- With --target-module-only, restrict fixture emission to binders
        -- DEFINED in the target module (by basename convention, mirroring
        -- GhcPipeline). Dep-module bindings (e.g. quasi-quoter internals
        -- from Tidepool.QQ) still participate in closed translation as
        -- dependencies — they just don't get their own fixtures, keeping
        -- the fixture sweep (and the JIT differential that walks it) to
        -- user-authored bindings.
        let targetModName = capitalize (takeBaseName path)
            keepBinder b
              | not (requestTargetModuleOnly args) = True
              | otherwise = case nameModule_maybe (idName b) of
                  Just m  -> moduleNameString (moduleName m) == targetModName
                  Nothing -> True
            allBinders = [ b | bind <- binds
                         , b <- case bind of
                                  NonRec b _ -> [b]
                                  Rec pairs  -> map fst pairs ]
            uniqueNames = Map.keys $ Map.fromList
              [(n, ()) | b <- allBinders
              , keepBinder b
              , let n = occNameString (nameOccName (idName b))
              , not ("$" `isPrefixOf` n)]
        -- The try-and-skip loop stays UPSTREAM of the shared writer (per
        -- 'translateTargetClosed''s haddock — this is a completely separate
        -- function/loop, never folded into it behind a policy flag). It
        -- forces each candidate's CBOR encoding here (not just its
        -- translation) so a lazy-thunk failure (e.g. unsupported FFI calls,
        -- the reason 'evaluate' is used at all) still causes a skip rather
        -- than aborting the whole sweep — 'writeClosedTargets' below has no
        -- per-target skip of its own and re-encodes every survivor for the
        -- actual write.
        closedTargets <- foldM (\acc name -> do
          compileAttempt <- try $ do
            closed@ClosedModule { cmNodes = nodes, cmUnresolved = unresolved } <- translateModuleClosed hscEnv binds name
            if not (null unresolved) then do
              let names = map (\uv -> uvModule uv ++ "." ++ uvName uv) unresolved
              hPutStrLn stderr $ "  SKIPPED (" ++ name ++ "): unresolved external(s): " ++ unwords names
              return Nothing
            else do
              _ <- evaluate (BS.length (encodeTree nodes))
              return (Just closed)
          case compileAttempt of
            Left (e :: SomeException) -> do
              hPutStrLn stderr $ "  SKIPPED (" ++ name ++ "): " ++ show e
              return acc
            Right Nothing -> return acc
            Right (Just closed) -> return (acc ++ [(name, name, closed)])
          ) [] uniqueNames
        -- Validate and emit all surviving fixtures through the shared writer.
        void $ writeClosedTargets timing outDir binds tycons preparedConstructors mCapturedTy warnTexts closedTargets
        pruneAllClosedArtifacts outDir (map (\(_, outFileBase, _) -> outFileBase) closedTargets)

      (Just targetName, False) ->
        -- Whole-module mode: serialize all bindings as nested lets around the
        -- target (shared with the session path; see 'writeWholeModuleClosed').
        -- File base name matches the lookup name here (the general CLI
        -- contract: --target foo produces foo.cbor). This is the branch the
        -- self-iterating harness's full-compile path actually exercises
        -- (tidepool-harness/src/compile.rs passes --target, never
        -- --all-closed), so it's the one carrying translate/cbor_encode/write
        -- timing.
        void $ writeWholeModuleClosed timing outDir hscEnv binds tycons preparedConstructors mCapturedTy warnTexts targetName targetName

      (Nothing, False) -> do
        -- Per-binding mode (original behavior). NOT unified with
        -- 'writeClosedTargets': 'translateBinds' translates each binding
        -- standalone, over a bare 'TransState' with no unresolved-id set and
        -- none of the runLLMTurn interception's aux var ids wired (see its
        -- definition in Translate.hs) — it never runs the
        -- 'resolveExternals'/reachability closure 'translateModuleClosed'
        -- does, so it produces no 'ClosedModule' and structurally has
        -- neither 'cmReachBinds' (required for metadata validation) nor any
        -- unresolved/dangling tracking (what 'cmVarNames' is built from).
        -- Routing it through the shared writer would mean rebuilding that
        -- closure machinery here, i.e. changing Translate.hs's translation
        -- semantics for this call site, which is outside this write path,
        -- and not a real unification if faked. It still gains the two
        -- things its own data honestly supports: a real 'hasIO' (was
        -- hardcoded False) and the asks.json sidecar's loud-absence
        -- contract — sites are structurally always empty on this path,
        -- since 'translateBind' never wires the runLLMTurn interception.
        let translated = translateBinds binds
            dedupd = dedup Map.empty translated
        mapM_ (\(name, nodes) -> do
          let cbor = encodeTree nodes
          let outFile = outDir </> cborFileName name
          BS.writeFile outFile cbor
          hPutStrLn stderr $ "  Wrote: " ++ outFile ++ " (" ++ show (Seq.length nodes) ++ " nodes, " ++ show (BS.length cbor) ++ " bytes)"
          ) dedupd

        -- Write DataCon metadata: merge TyCon-derived + usage-derived + transitive + wired-in
        let tyconMeta = collectDataCons tycons
            usedMeta = collectUsedDataCons binds
            transitiveMeta = collectTransitiveDCons tycons binds
            wiredInMeta = wiredInDataCons
            -- Highest priority first; mergeMetaPreserving keeps colliding
            -- (same-varId, different-qualified-name) entries distinct so the
            -- loader rejects them loudly instead of one silently winning.
            allMeta = mergeMetaPreserving
                        [ wiredInMeta, tyconMeta, usedMeta, transitiveMeta ]
            hasIO = any (targetBindingHasIO binds . fst) dedupd
        let metaCbor = encodeMetadata allMeta hasIO mCapturedTy [] warnTexts []
        let metaFile = outDir </> "meta.cbor"
        BS.writeFile metaFile metaCbor
        hPutStrLn stderr $ "  Wrote: " ++ metaFile ++ " (" ++ show (length allMeta) ++ " entries, " ++ show (BS.length metaCbor) ++ " bytes)"

        let asksFile = outDir </> "asks.json"
        writeFile asksFile (renderAsksJson [])
        hPutStrLn stderr $ "  Wrote: " ++ asksFile ++ " (0 sites)"

    writePreparedArtifacts outDir preparedArtifacts

  reportDiags res

trySynchronous :: IO a -> IO (Either SomeException a)
trySynchronous action = do
  result <- try action
  case result of
    Left exception -> case fromException exception :: Maybe SomeAsyncException of
      Just async -> throwIO async
      Nothing -> pure (Left exception)
    Right value -> pure (Right value)

data PreparedArtifact = PreparedArtifact
  { paTarget :: String
  , paBytes :: BS.ByteString
  , paConstructors :: [DataCon]
  }

-- Project before writing either engine's artifacts so the shared constructor
-- table includes exactly the GHC constructors admitted by prepared execution.
prepareArtifacts :: RecoveryCaches -> FilePath -> HscEnv -> [PreparedModule] -> [String] -> [String]
  -> Map.Map SymbolIdentity Word64 -> IO [PreparedArtifact]
prepareArtifacts _ _ _ _ [] _ _ = pure []
prepareArtifacts caches input hscEnv modules targets auxiliaryRoots retainedGenerations = do
  timing <- readTimingEnabled
  formattingAuthority <- resolveFormattingAuthority hscEnv
  textAuthority <- resolveTextPackageUnit hscEnv
  source <- readFile input
  let targetModule = fromMaybe (capitalize (takeBaseName input)) (extractModuleName source)
      matching = [prepared | prepared <- modules,
        moduleNameString (moduleName (pmModule prepared)) == targetModule]
  preparedModule <- case matching of
    [value] -> pure value
    values -> ioError (userError ("prepared target module selection was not unique: " ++ show (length values)))
  (architecture, abi) <- case SystemInfo.arch of
    "x86_64" -> pure (X86_64, "sysv64")
    "aarch64" -> pure (Aarch64, "aapcs64")
    other -> ioError (userError ("prepared execution is not configured for " ++ other))
  forM targets $ \target -> do
    let entry = SymbolIdentity
          (T.pack (unitString (moduleUnit (pmModule preparedModule))))
          (T.pack targetModule) "value" (T.pack target) Nothing
        context = ProjectionContext
          { projectionProfile = "ghc-9.12-prepared-stg"
          , projectionToolchain = "ghc-9.12.2"
          , projectionTarget = TargetDescriptor architecture LittleEndian 64 64 abi []
          , projectionRetainedGenerations = retainedGenerations
          , projectionEntry = entry
          , projectionAuxiliaryRoots =
              [ SymbolIdentity (T.pack (unitString (moduleUnit (pmModule preparedModule))))
                  (T.pack targetModule) "value" (T.pack root) Nothing
              | root <- auxiliaryRoots ]
          , projectionFormattingAuthority = formattingAuthority
          , projectionTextUnit = textAuthority
          }
    -- Three flat phases, one row each per target (see Tidepool.Timing).
    -- Projection is pure and only forced to weak head normal form here, so
    -- part of its cost lands in 'prepared_encode'; read the two together.
    recovered <- timePhase timing "prepared_recover"
      (recoverPreparedClosure hscEnv (rcFatIface caches) (rcOwnerIface caches)
        (rcPreparedBodies caches) context modules)
    reportRecoveryResiduals target (closureFailures recovered)
    (program, constructors) <- timePhase timing "prepared_project" $
      case projectPreparedTargetWithConstructors context (closureModules recovered) of
        -- A reachable polymorphic typed site is the author's source error.
        Left (RejectedTypedSite message) -> throwIO (SourceRejection (T.unpack message))
        Left failure -> ioError (userError ("prepared projection failed: " <> show failure))
        Right projected -> evaluate projected
    bytes <- timePhase timing "prepared_encode" (evaluate (encodeWireProgram program))
    pure (PreparedArtifact target bytes constructors)

-- | Filter the standard prepared-turn auxiliary root names
-- ('preparedResumeTargetName', 'preparedDecodeTargetName') down to those the
-- module actually defines as top-level binders. Shared by 'processFile' and
-- 'runTurnMode' so both admit @__resume@\/@__decodeValue@ as auxiliary roots
-- exactly when a compiled module (e.g. the harness's fused turn module)
-- defines them, and admit nothing extra for an ordinary module with no
-- scaffold.
standardAuxiliaryRoots :: [CoreBind] -> [String]
standardAuxiliaryRoots binds =
  [ name
  | name <- [ preparedResumeTargetName, preparedDecodeTargetName
            , preparedApplyEntryTargetName, preparedApplyValueTargetName
            ]
  , name `Set.member` topLevelNames
  ]
  where
    topLevelNames = Set.fromList
      [ occNameString (nameOccName (idName b))
      | bind <- binds
      , b <- case bind of
               NonRec b' _ -> [b']
               Rec pairs   -> map fst pairs
      ]

writePreparedArtifacts :: FilePath -> [PreparedArtifact] -> IO ()
writePreparedArtifacts outDir artifacts = forM_ artifacts $ \artifact -> do
  let output = outDir </> paTarget artifact ++ ".prepared.cbor"
  BS.writeFile output (paBytes artifact)
  hPutStrLn stderr $ "  Wrote: " ++ output ++ " (prepared execution)"

reportRecoveryResiduals :: String -> [RecoveryFailure] -> IO ()
reportRecoveryResiduals _ [] = pure ()
reportRecoveryResiduals target failures =
  hPutStrLn stderr $ "  Prepared recovery residuals (" ++ target ++ "): "
    ++ show failures

-- | Turn mode (@--turn@): classify the raw
-- turn text (or accept a caller-supplied @--turn-verdict@), splice the
-- matching template, compile through the resident session path
-- ('runPipelineSession' \/ 'writeWholeModuleClosed'), and write the rich
-- 'TurnOut' result as CBOR (@--turn-out@). A @decl@ verdict never compiles: its
-- 'toDeclItems' come from a whole-module parse over the turn's OWN spliced
-- scratch module ('extractBindersNamed', exact-name match — see
-- @--turn-template decl=<file>@ below), never from the single-statement
-- parse that serves the verdict — a decl-batch caller
-- (@--turn-verdict decl@ over N declarations joined into one module) has no
-- single statement to parse, only the module.
--
-- The template kind selected for a @bind@ verdict is either @bind@ (at least
-- one bound name) or @binddiscard@ (a bind that binds no name, e.g.
-- @_ <- e@) — mirroring the Rust @TemplateSelector@'s four-shape split. A
-- @binddiscard@ turn compiles but is routed entirely around the
-- session-bind artifacts (no @--bind-gen@\/@--session-root@ requirement, no
-- 'mkBoundBinders', no thin-iface write): it runs for effect and discards,
-- so it reaches 'TBind' with empty binders and an empty bound-binder list,
-- same shape a caller already handles for any other zero-binder bind.
runTurnMode
  :: Compiler -> RecoveryCaches -> WorkerRequest -> FilePath -> IO ExitCode
runTurnMode compiler caches args path = do
  timing <- readTimingEnabled
  hPutStrLn stderr $ "Processing (turn): " ++ path
  res <- timePhase timing "total" $ try $ do
    turnSrc   <- readFile path
    let templates = requestTurnTemplates args
    mVerdict  <- traverse parseTurnVerdictArg (requestTurnVerdict args)
    -- 'extractStmtBinders' emits no phases of its own. This mode times it as
    -- the single @classify@ phase, emitted
    -- only on the branch that actually classifies. With @--turn-verdict@
    -- supplied nothing is parsed, and an absent @classify@ row is the
    -- honest report rather than a phantom 0ms line.
    sb        <- maybe (timePhase timing "classify" (extractStmtBinders turnSrc)) return mVerdict
    let outDir     = fromMaybe (takeDirectory path </> takeBaseName path ++ "_cbor") (requestOutDir args)
        bindersStr = fromMaybe
          (intercalate ", " (sbBinders sb))
          (requestTurnPin args)
        -- Splice @tmplFile@ against the turn text, write the spliced module
        -- to a scratch file under 'outDir', and return it alongside the
        -- module name derived from its own @module X where@ header. The
        -- scratch file's basename must match that header — 'runPipelineSession'
        -- looks up the compiled module by @capitalize (takeBaseName path)@
        -- (GhcPipeline.hs) exactly as 'tidepool_runtime::extract_module_name'
        -- does today for the existing two-spawn wrap_* templates
        -- (session.rs), which this mode's templates carry over unchanged.
        spliceInto :: FilePath -> IO (String, String, FilePath)
        spliceInto tmplFile = do
          tmplSrc <- readFile tmplFile
          writeSpliced (spliceTemplate tmplSrc turnSrc bindersStr)
        writeSpliced spliced = do
          let modName = fromMaybe "Input" (extractModuleName spliced)
          createDirectoryIfMissing True outDir
          let modulePath = outDir </> modName ++ ".hs"
          writeFile modulePath spliced
          -- Overwritten before every ordered compile attempt. The Rust turn
          -- boundary reads this only on failure so frontend diagnostics use
          -- the exact module GHC last saw, never an unspliced template guess.
          writeFile (outDir </> "turn-attempt.hs") spliced
          return (spliced, modName, modulePath)
    turnOut <- case sbKind sb of
      KDecl -> do
        tmplFile <- case lookup (templateSelectorWireName SDecl) templates of
          Just f  -> return f
          Nothing -> error "--turn: no --turn-template for kind decl"
        tmplSrc <- readFile tmplFile
        declarationSource <- declarationSourceWithTemplate tmplSrc turnSrc
          >>= either throwCellSplitError pure
        spliced <- either fail pure (renderDeclarationForTemplate tmplSrc declarationSource)
        (_spliced, modName, modulePath) <- writeSpliced spliced
        items <- extractBindersNamed modulePath (requestIncludes args) modName
        let binders = if null (sbBinders sb)
                        then map (T.pack . exportItemName) items
                        else map T.pack (sbBinders sb)
        return (TDecl binders items declarationSource)
      kind -> do
        -- Four-shape selection (protocol note, "the verdict space has four
        -- shapes, not three"): a bind that binds no name selects its own
        -- template kind and skips the session-bind artifacts entirely —
        -- 'templateSelectorForVerdict' mirrors Rust's
        -- 'TemplateSelector::for_verdict' exactly.
        let selector = templateSelectorForVerdict kind (sbBinders sb)
        let scope = scopeFromWorkerRequest args
            matching = [f | (name, f) <- templates, name == templateSelectorWireName selector]
            -- A prepared turn is one PreparedStg compile: it yields the same
            -- Core result the legacy path reads, plus the prepared modules.
            compileTurn modulePath
              | requestPreparedTurn args = do
                  prepared <- compiler PreparedStg (Map.keysSet (requestRetainedGenerations args)) GeneralCompile (Just scope) modulePath (requestIncludes args) (requestBuildProductsDir args)
                  return (pprPipelineResult prepared, pprModules prepared)
              | otherwise = do
                  legacy <- compiler LegacyCore Set.empty GeneralCompile (Just scope) modulePath (requestIncludes args) (requestBuildProductsDir args)
                  return (legacy, [])
            compileVariants _ [] = error ("--turn: no --turn-template for kind " ++ templateSelectorWireName selector)
            compileVariants index (tmplFile:rest) = do
              (spliced, _modName, modulePath) <- spliceInto tmplFile
              attempted <- try (compileTurn modulePath)
              case attempted of
                Right (result, preparedModules) -> return (index, spliced, modulePath, result, preparedModules)
                Left err@(_ :: SomeException) -> case (fromException err :: Maybe SourceError, rest) of
                  (Just _, _ : _) -> compileVariants (index + 1) rest
                  _               -> throwIO err
        (variant, spliced, compiledPath, result, preparedModules) <- compileVariants (0 :: Int) matching
        let binds       = prBinds result
            hscEnv      = prHscEnv result
            mCapturedTy = fmap T.pack (prCapturedType result)
            warnTexts   = map T.pack (prWarnings result)
        -- The Core binding to look up. Scaffold-reserved by default, but a
        -- caller whose template names its own target says so with --target.
        -- The output file base
        -- stays "result" regardless — every Rust caller reads result.cbor.
        let targetName = fromMaybe scaffoldTargetName (requestTarget args)
        -- Projection remains outside compileVariants: a prepared rejection
        -- cannot select a Core template fallback. Its entry is the settled
        -- scaffold, and its constructors join the shared metadata before write.
        preparedArtifacts <- if requestPreparedTurn args
          then prepareArtifacts caches compiledPath hscEnv preparedModules
                 [preparedScaffoldTargetName] (standardAuxiliaryRoots binds) (requestRetainedGenerations args)
          else pure []
        asksSites <- writeWholeModuleClosed timing outDir hscEnv binds (prTyCons result)
          (concatMap paConstructors preparedArtifacts) mCapturedTy warnTexts targetName scaffoldOutputBase
        writePreparedArtifacts outDir preparedArtifacts
        let wrapped = T.pack spliced
        case selector of
          SBind -> do
            g    <- requireArg "--bind-gen"     (requestBindGen args)
            root <- requireArg "--session-root" (requestSessionRoot args)
            bbs  <- mkBoundBinders (sbBinders sb) g root result
            return (TBind (map T.pack (sbBinders sb)) variant bbs asksSites wrapped)
          SBindDiscard -> return (TBind [] variant [] asksSites wrapped)
          SExpr -> return (TExpr variant asksSites wrapped)
          SDecl -> error ("--turn: unexpected verdict kind: " ++ templateSelectorWireName selector)
    outFile <- requireArg "--turn-out" (requestTurnOut args)
    let cbor = encodeTurnOut turnOut
    BS.writeFile outFile cbor
    hPutStrLn stderr $ "  Wrote: " ++ outFile ++ " (" ++ show (BS.length cbor) ++ " bytes)"
  reportDiags res

-- | Block classify mode (@--classify@):
-- classify EVERY positional file in 'requestFiles' with ONE GHC session boot
-- ('classifyBlock'), in argv order, and write the verdicts to
-- @--classify-out@. Serves @tidepool-repl@'s block runner, which segments a
-- block into decl runs before compiling any item and so needs every verdict
-- up front — one spawn for the whole block instead of one classify spawn per
-- item.
runClassifyMode :: Bool -> WorkerRequest -> IO ExitCode
runClassifyMode timing args =
  -- No live GHC session exists in this parse-only mode, so the caught
  -- exception below always takes 'reportDiags''s 'Nothing' branch (never a
  -- 'SourceError' to distinguish).
  timePhase timing "total" $
    try
      ( do
          out      <- requireArg "--classify-out" (requestClassifyOut args)
          srcs     <- mapM readFile (requestFiles args)
          verdicts <- classifyBlock timing srcs
          writeFile out (renderVerdictsJson verdicts)
          hPutStrLn stderr $ "  Wrote: " ++ out ++ " (" ++ show (length verdicts) ++ " verdicts)"
      )
      >>= reportDiags

-- | Split, classify, and typecheck one notebook cell in a single worker
-- request. Rust authors the module template (scope/import/effect-row policy);
-- GHC owns every Haskell decision and returns post-zonk statement binder pins.
runCellMode :: Compiler -> WorkerRequest -> FilePath -> IO ExitCode
runCellMode compiler args cellPath = do
  res <- try $ do
    cellSource <- readFile cellPath
    templatePath <- requireArg "--cell-template" (requestCellTemplate args)
    template <- readFile templatePath
    initialPlan <- analyzeCell template cellSource >>= either throwCellSplitError pure
    initialSource <- either fail pure (renderCellCheckSource template initialPlan)
    let outDir = fromMaybe
          (takeDirectory cellPath </> takeBaseName cellPath ++ "_cell")
          (requestOutDir args)
        moduleName' = fromMaybe "CellCheck" (extractModuleName initialSource)
        modulePath = outDir </> moduleName' ++ ".hs"
        scope = if hasSessionScope args
          then Just (scopeFromWorkerRequest args)
          else Nothing
    createDirectoryIfMissing True outDir
    out <- requireArg "--cell-out" (requestCellOut args)
    (analyzed, provisional) <- checkCellInstances (\plan -> do
      rendered <- either fail pure (renderCellCheckSource template plan)
      writeFile modulePath rendered
      -- Preserve GHC's source plan even when checking reports diagnostics.
      BS.writeFile out (encodeCellOut plan [] rendered)
      compiler LegacyCore Set.empty GeneralCompile scope modulePath (requestIncludes args) (requestBuildProductsDir args)) initialPlan
    checkedSource <- either fail pure (renderCellCheckSource template analyzed)
    (finalPlan, finalSource, compiled) <- if null (cellPlanDisplayTargets analyzed)
      then pure (analyzed, checkedSource, provisional)
      else do
        contextDeclarations <- cellDisplayDeclarations DisplayInstanceContexts provisional analyzed
        let contextual = installCellDisplayDeclarations contextDeclarations analyzed
        contextualSource <- either fail pure (renderCellCheckSource template contextual)
        writeFile modulePath contextualSource
        contextChecked <- compiler LegacyCore Set.empty GeneralCompile scope modulePath (requestIncludes args) (requestBuildProductsDir args)
        declarations <- cellDisplayDeclarations DisplayInstanceFields contextChecked analyzed
        let finalized = installCellDisplayDeclarations declarations analyzed
        finalizedSource <- either fail pure (renderCellCheckSource template finalized)
        writeFile modulePath finalizedSource
        finalizedResult <- compiler LegacyCore Set.empty GeneralCompile scope modulePath (requestIncludes args) (requestBuildProductsDir args)
        pure (finalized, finalizedSource, finalizedResult)
    -- Statement preparation checks these rendered pins in their actual value
    -- modules before any declaration commits or effect runs.
    BS.writeFile out
      (encodeCellOut finalPlan (prCheckedBinderPins compiled) finalSource)
  reportDiags res

-- | Parse one raw @--turn-verdict kind[:name,name…]@ argument into the same
-- 'StmtBinders' shape 'extractStmtBinders' would have produced, so the rest of
-- 'runTurnMode' never has to distinguish a supplied verdict from a parsed one.
-- @kind@ goes through 'parseTurnKind', which fails loudly (caught by this
-- mode's surrounding @try@, same as any other extraction failure) on
-- anything but the three wire-name strings the Rust caller ever forwards.
parseTurnVerdictArg :: String -> IO StmtBinders
parseTurnVerdictArg s = case break (== ':') s of
  (kind, "")      -> return (StmtBinders (parseTurnKind kind) [] [])
  (kind, ':' : ns) -> return (StmtBinders (parseTurnKind kind) (splitComma ns) [])
  _               -> error ("--turn: malformed --turn-verdict: " ++ s)

splitComma :: String -> [String]
splitComma s = case break (== ',') s of
  (a, [])       -> [a]
  (a, _ : rest) -> a : splitComma rest

requireArg :: String -> Maybe a -> IO a
requireArg flag = maybe (error ("required argument missing: " ++ flag)) return

-- | Deduplicate binding names by appending _1, _2, etc. for collisions.
dedup :: Map.Map String Int -> [(String, a)] -> [(String, a)]
dedup _ [] = []
dedup seen ((name, val) : rest) =
  case Map.lookup name seen of
    Nothing -> (name, val) : dedup (Map.insert name 1 seen) rest
    Just n  -> (name ++ "_" ++ show n, val) : dedup (Map.insert name (n + 1) seen) rest
