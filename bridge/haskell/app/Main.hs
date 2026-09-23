{-# LANGUAGE RankNTypes #-}

module Main where

import System.Environment (getArgs)
import System.FilePath (takeBaseName, takeDirectory, takeFileName, (</>))
import System.Directory (createDirectoryIfMissing, setCurrentDirectory)
import qualified Data.ByteString as BS
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import Control.Exception
  ( evaluate, try, throwIO, SomeAsyncException, SomeException, Exception
  , fromException, toException, IOException )
import Data.IORef (newIORef, readIORef, writeIORef)
import Data.List (intercalate)
import Data.Maybe (fromMaybe, mapMaybe, isJust)
import Data.Word (Word64)
import Control.Monad (foldM, forM, forM_)
import System.Exit (ExitCode(..), exitWith)
import System.IO (hPutStrLn, stderr, stdin, stdout, hSetBinaryMode, hSetEncoding, utf8)
import qualified System.Info as SystemInfo

import GHC.Types.SourceError (SourceError)
import GHC (Module, ModuleName, moduleName, moduleNameString, moduleUnit)
import GHC.Driver.Env (HscEnv)
import GHC.Unit.Types (unitString)
import GHC.Core (Bind(..), CoreBind)
import GHC.Core.DataCon (DataCon)
import GHC.Core.TyCon (TyCon)
import GHC.Types.Name (nameOccName)
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
  , StmtBinders(..), TurnOut(..), renderAskJson, renderVerdictsJson )
import Tidepool.GhcPipeline
  ( PipelineSelection(..), PreparedPipelineResult(..), CheckedEnvironmentResult(..)
  , runPipelineSessionSelected, CompilePurpose(..), PipelineResult(..)
  , withResidentPipelineSelectedRequests, CellDisplayPass(..), cellDisplayDeclarations, checkCellInstances
  , cellExpressionPlans
  , satisfiesCapturedConstraint, stripMonadHead )
import Tidepool.ExecutionEncode (encodeWireProgram)
import Tidepool.ExecutionProjection (ProjectionContext(..), ProjectionError(..), prepareProjection, projectSelected, resolveTextPackageUnit)
import Tidepool.PreparedFormatting (resolveFormattingAuthority)
import Tidepool.PreparedTime (resolveTimeAuthority)
import Tidepool.PreparedJson (resolveJsonAuthority)
import Tidepool.ExecutionSchema
  ( Architecture(..), Endianness(..), SymbolIdentity(..), TargetDescriptor(..)
  , WireProgram(..), SiteRow(..) )
import qualified Tidepool.EffectSchema
import Tidepool.PreparedStg
  ( PreparedModule(..), PreparedBodyCache, newPreparedBodyCache
  , evictPreparedBodyMatching )
import Tidepool.PreparedRecovery
  ( RecoveryFailure, RecoveredClosure(..), newPreparedRecovery )
import qualified Tidepool.WorkerServer as WorkerServer
import Tidepool.DiagJson
  ( ReportOutcome(..), DiagSeverity(..), Diag(..), SourceRejection(..)
  , diagsFromSourceError, diagFromException, renderDiagsJson )
import Tidepool.ExtractUtil (capitalize)
import Tidepool.ExtractRequest (InspectionRequest(..), WorkerRequest(..), workerRequestFromArgv, workerRequestFlag)
import Tidepool.Introspection (InspectionResult(..), encodeInspectionResults, runInspection)
import Tidepool.Session
  ( SessionScope(..), preparedScaffoldTargetName, preparedResumeTargetName
  , preparedApplyEntryTargetName, preparedApplyValueTargetName
  , parseSessionModule )
import Tidepool.FatIface
  ( FatIfaceCache, newFatIfaceCache, evictFatIfaceMatching
  , OwnerInterfaceCache, newOwnerInterfaceCache, evictOwnerInterfaceMatching )
import Tidepool.SessionArtifacts
  ( mkBoundBinders, parseValModule )
import Tidepool.Metadata
  ( collectDataCons, dcToMeta, mergeMetaPreserving, targetBindingHasIO
  , wiredInDataCons )
import Tidepool.CborEncode (encodeMetadata, encodeTurnOut, encodeCellOut)
import Tidepool.Timing (readTimingEnabled, timePhase)
import Tidepool.TurnSource (extractModuleName, spliceTemplate)
import Tidepool.DependencyEvidence
  ( DependencyEvidence, renderDependencyEvidence, revalidateDependencyEvidence )

renderAsksJson :: [Tidepool.EffectSchema.YieldSite] -> String
renderAsksJson sites = "[" ++ intercalate "," (map renderAskJson sites) ++ "]"

-- | The retained-generation 'Set.Set' threads a request's
-- @--retained-generation@ symbols (see 'Tidepool.RetainedUnfoldings') into
-- the compile call so an imported retained-generation symbol's unfolding is
-- withheld from GHC's own simplifier rather than being inlined into a
-- consumer compiled in the same session. Every call site outside
-- 'processFile''s 'PreparedStg' compile passes 'Set.empty' (a true no-op):
-- only a prepared-STG compile recovers and persists retained-generation
-- 'GlobalDecl' references; metadata checks have nothing to withhold. The resident-daemon path ('withResidentPipelineSelectedRequests',
-- used only behind @--worker-loop-v2@) honors this parameter per compile too,
-- via a single installed plugin that reads a transaction-local 'IORef' cell.
type Compiler =
  forall result. PipelineSelection result
  -> Set.Set SymbolIdentity
  -> CompilePurpose
  -> Maybe SessionScope
  -> FilePath
  -> [FilePath]
  -> Maybe FilePath
  -> IO result

-- | Compiler-owned recovery caches. The resident pipeline invokes their
-- scoped eviction callback when a transaction ends; one-shot compilation
-- owns fresh caches for its invocation.
data RecoveryCaches = RecoveryCaches
  { rcFatIface :: FatIfaceCache
  , rcOwnerIface :: OwnerInterfaceCache
  , rcPreparedBodies :: PreparedBodyCache
  }

freshRecoveryCaches :: IO RecoveryCaches
freshRecoveryCaches = RecoveryCaches
  <$> newFatIfaceCache <*> newOwnerInterfaceCache <*> newPreparedBodyCache

-- | True for a 'Module' whose cached recovery state must not survive past
-- this transaction: one of the transaction's target modules, or any
-- @Tidepool.Session.*@ module (both mirror 'Tidepool.GhcPipeline.sanitizeMemo''s
-- own predicate for 'GutsMemo', over the same 'ModuleName').
staleRecoveryModule :: ModuleName -> Module -> Bool
staleRecoveryModule targetModName' owner =
  moduleName owner == targetModName'
    || isJust (parseSessionModule (moduleNameString (moduleName owner)))

evictRecoveryCaches :: RecoveryCaches -> ModuleName -> IO ()
evictRecoveryCaches caches targetModName' = do
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
  if rawWorkerRequest == ["--print-worker-request-flag"]
    then putStrLn workerRequestFlag >> exitWith ExitSuccess
  else if rawWorkerRequest == ["--worker-loop-v2"]
    then do
      hSetBinaryMode stdin True
      hSetBinaryMode stdout True
      caches <- freshRecoveryCaches
      withResidentPipelineSelectedRequests [] (evictRecoveryCaches caches) $ \runRequest ->
        WorkerServer.runWorkerLoop $ \serveTransaction ->
          runRequest $ \compiler ->
            serveTransaction (\cwd argv ->
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
  -- files — see the module doc there and exomonad/harness/src/timing.rs.
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
        | isJust (requestInspectTypeBatch args)
          && not (length (requestInspections args) > 1 && all isInspectionTypeQuery (requestInspections args))
                                                  -> reportDiags (Left (toException (userError "inspection type batch requires at least two type queries and no other query kinds")))
        | requestActivationPreview args && (not (requestTurn args) || not (isJust (requestTurnVerdict args)))
                                                  -> reportDiags (Left (toException (userError "activation requires a prepared turn with a generated bind verdict")))
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
    results <- case requestInspectTypeBatch args of
      Nothing -> runSingletons scope queries
      Just batchPath
        | length queries > 1 && all isInspectionTypeQuery queries -> do
            -- Validate producer infrastructure outside the SourceError catch:
            -- only a compiler rejection of readable authored source may fall
            -- back to the preserved singleton modules.
            _ <- BS.readFile batchPath
            compiled <- try (compiler CheckedEnvironment Set.empty GeneralCompile scope batchPath (requestIncludes args) (requestBuildProductsDir args))
            case compiled of
              Left exception
                | requestInspectionStrict args -> throwIO exception
                | otherwise -> case fromException exception of
                    Just (_ :: SourceError) -> runSingletons scope queries
                    Nothing -> throwIO exception
              Right successful -> inspect successful queries
        | otherwise -> fail "inspection type batch requires at least two type queries and no other query kinds"
    BS.writeFile out (encodeInspectionResults results)
  reportDiags res
  where
    inspect successful queries = runInspection
      (crHscEnv successful)
      (crTargetTcGblEnv successful)
      (crTargetRdrEnv successful)
      (crInspectionProbes successful)
      queries

    -- A source path identifies an exact generated source in this request.
    -- The producer shares it only for queries with identical scope/imports;
    -- wildcard-normalized searches use their own source and compile purpose.
    runSingletons scope queries = snd <$> foldM inspectNext (Map.empty, []) (zip (requestFiles args) queries)
      where
        inspectNext (environments, answers) (path, query) = do
          let purpose = case query of
                InspectTypeSearch _ -> LookupTypeCompile
                _ -> GeneralCompile
              key = (purpose, path)
          compiled <- case Map.lookup key environments of
            Just previous -> pure previous
            Nothing -> try (compiler CheckedEnvironment Set.empty purpose scope path (requestIncludes args) (requestBuildProductsDir args))
          result <- case compiled of
            Left exception
              | requestInspectionStrict args -> throwIO exception
              | otherwise -> case fromException exception of
                  Just (sourceError :: SourceError) ->
                    pure [InspectionRejected (renderInspectionDiagnostics sourceError)]
                  Nothing -> throwIO exception
            Right successful -> inspect successful [query]
          pure (Map.insert key compiled environments, answers ++ result)

isInspectionTypeQuery :: InspectionRequest -> Bool
isInspectionTypeQuery query = case query of
  InspectTypeOf _ -> True
  _ -> False

renderInspectionDiagnostics :: SourceError -> String
renderInspectionDiagnostics = intercalate "\n" . map render . diagsFromSourceError
  where
    render diagnostic = location diagnostic ++ dMessage diagnostic
    location diagnostic = case dFile diagnostic of
      Just (file, line, column, _, _) -> file ++ ":" ++ show line ++ ":" ++ show column ++ ": "
      Nothing -> ""


-- | Prepend the harness language profile to scratch input copies. Inspection
-- requests profile every singleton fallback and their optional batch source;
-- other modes compile only the first input. Putting the profile in source
-- keeps it visible to GHC downsweep and source-based cache keys. Caller files
-- are never modified; diagnostics are shifted by the inserted line.
spliceHarnessProfilePragma :: WorkerRequest -> IO WorkerRequest
spliceHarnessProfilePragma args = case requestFiles args of
  [] -> pure args
  (file : rest) -> do
    let outDir = fromMaybe (takeDirectory file </> takeBaseName file ++ "_cbor") (requestOutDir args)
        profileCopy directory sourcePath = do
          source <- readFile sourcePath
          let scratchPath = directory </> takeFileName sourcePath
          createDirectoryIfMissing True directory
          writeFile scratchPath (harnessProfilePragmaLine ++ "\n" ++ source)
          pure scratchPath
    if null (requestInspections args)
      then do
        scratchPath <- profileCopy outDir file
        pure args { requestFiles = scratchPath : rest }
      else do
        (_, files) <- foldM
          (\(copies, paths) (index, sourcePath) -> do
            copy <- case Map.lookup sourcePath copies of
              Just existing -> pure existing
              Nothing -> profileCopy (outDir </> "inspection-query-" ++ show index) sourcePath
            pure (Map.insert sourcePath copy copies, paths ++ [copy]))
          (Map.empty, []) (zip [0 :: Int ..] (file : rest))
        batch <- case requestInspectTypeBatch args of
          Nothing -> pure Nothing
          Just sourcePath -> Just <$> profileCopy (outDir </> "inspection-type-batch") sourcePath
        pure args
          { requestFiles = files
          , requestInspectTypeBatch = batch
          }

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

    let outDir = case mOutDir of
          Just dir -> dir
          Nothing  -> takeDirectory path </> takeBaseName path ++ "_cbor"
    createDirectoryIfMissing True outDir

    let preparedTargets = case requestTargets args of
          targets@(_ : _) -> targets
          [] -> maybe [] pure mTarget
    preparedArtifacts <- prepareArtifacts caches path hscEnv (pprModules prepared) preparedTargets
      (standardAuxiliaryRoots binds) (requestRetainedGenerations args)
    if null preparedArtifacts
      then ioError (userError "prepared extraction requires --target or --targets")
      else timePhase timing "prepared_sidecars" $ writePreparedSidecars SeparateYieldSites outDir binds tycons mCapturedTy warnTexts preparedArtifacts

    timePhase timing "prepared_write" $ writePreparedArtifacts outDir preparedArtifacts
    writeDependencyEvidence outDir (pprDependencies prepared)

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
  , paYieldSites :: [Tidepool.EffectSchema.YieldSite]
  }

-- Project before writing artifacts so the shared constructor
-- table includes exactly the GHC constructors admitted by prepared execution.
prepareArtifacts :: RecoveryCaches -> FilePath -> HscEnv -> [PreparedModule] -> [String] -> [String]
  -> Map.Map SymbolIdentity Word64 -> IO [PreparedArtifact]
prepareArtifacts _ _ _ _ [] _ _ = pure []
prepareArtifacts caches input hscEnv modules targets@(firstTarget : _) auxiliaryRoots retainedGenerations = do
  timing <- readTimingEnabled
  formattingAuthority <- timePhase timing "formatting_authority" $ resolveFormattingAuthority hscEnv
  timeAuthority <- timePhase timing "time_authority" $ resolveTimeAuthority hscEnv
  jsonAuthority <- timePhase timing "json_authority" $ resolveJsonAuthority hscEnv
  textAuthority <- timePhase timing "text_authority" $ resolveTextPackageUnit hscEnv
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
  let contextFor target =
        let entry = SymbolIdentity
              (T.pack (unitString (moduleUnit (pmModule preparedModule))))
              (T.pack targetModule) "value" (T.pack target) Nothing
        in ProjectionContext
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
          , projectionTimeAuthority = timeAuthority
          , projectionJsonAuthority = jsonAuthority
          , projectionTextUnit = textAuthority
          }
  recover <- newPreparedRecovery hscEnv (rcFatIface caches) (rcOwnerIface caches)
    (rcPreparedBodies caches) (contextFor firstTarget) modules
  forM targets $ \target -> do
    let context = contextFor target
    -- Three flat phases, one row each per target (see Tidepool.Timing).
    -- Selection forces its complete identity/binding inventory. Lowering is
    -- lazy, so encoding owns and forces lowering plus wire serialization.
    recovered <- timePhase timing "prepared_recover"
      (recover (projectionEntry context))
    reportRecoveryResiduals target (closureFailures recovered)
    selected <- timePhase timing "prepared_project" $
      requireProjection (prepareProjection context (closureModules recovered))
    (program, constructors, bytes) <- timePhase timing "prepared_encode" $ do
      (program, constructors) <- requireProjection (projectSelected selected)
      bytes <- evaluate (encodeWireProgram program)
      pure (program, constructors, bytes)
    let admitted = Set.fromList (map siteId (programSites program))
        yieldSites =
          [ site
          | preparedModule' <- closureModules recovered
          , site <- pmYieldSites preparedModule'
          , Tidepool.EffectSchema.ysSite site `Set.member` admitted
          ]
    pure (PreparedArtifact target bytes constructors yieldSites)

requireProjection :: Either ProjectionError a -> IO a
requireProjection = \case
  Left (RejectedTypedSite message) -> throwIO (SourceRejection (T.unpack message))
  Left failure -> ioError (userError ("prepared projection failed: " <> show failure))
  Right projected -> evaluate projected

-- | Filter the standard prepared-turn auxiliary root names
-- ('preparedResumeTargetName' and the generic apply roots) down to those the
-- module actually defines as top-level binders. Shared by 'processFile' and
-- 'runTurnMode' so both admit the executable scaffold's auxiliary roots
-- exactly when a compiled module (e.g. the harness's fused turn module)
-- defines them, and admit nothing extra for an ordinary module with no
-- scaffold.
standardAuxiliaryRoots :: [CoreBind] -> [String]
standardAuxiliaryRoots binds =
  [ name
  | name <- [ preparedResumeTargetName, preparedApplyEntryTargetName
            , preparedApplyValueTargetName
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

writeDependencyEvidence :: FilePath -> DependencyEvidence -> IO ()
writeDependencyEvidence outDir evidence = do
  validateDependencyEvidence evidence
  writeFile (outDir </> "dependencies.json") (renderDependencyEvidence evidence)

validateDependencyEvidence :: DependencyEvidence -> IO ()
validateDependencyEvidence evidence = do
  unchanged <- revalidateDependencyEvidence evidence
  if unchanged
    then pure ()
    else ioError (userError "source changed while compiler artifacts were being published")

data YieldSiteDelivery = SeparateYieldSites | InlineYieldSites

writePreparedSidecars
  :: YieldSiteDelivery -> FilePath -> [CoreBind] -> [TyCon] -> Maybe T.Text -> [T.Text]
  -> [PreparedArtifact] -> IO ()
writePreparedSidecars delivery outDir binds tycons capturedType warnings artifacts = do
  let constructors = concatMap paConstructors artifacts
      metadata = mergeMetaPreserving
        [ wiredInDataCons, collectDataCons tycons, map dcToMeta constructors ]
      hasIO = any (targetBindingHasIO binds . paTarget) artifacts
      metaBytes = encodeMetadata metadata hasIO capturedType warnings
  BS.writeFile (outDir </> "meta.cbor") metaBytes
  case delivery of
    InlineYieldSites -> pure () -- TurnOut carries the same typed sites.
    SeparateYieldSites -> do
      let multiple = length artifacts > 1
      forM_ artifacts $ \artifact -> do
        let asksName = if multiple then paTarget artifact ++ ".asks.json" else "asks.json"
        writeFile (outDir </> asksName) (renderAsksJson (paYieldSites artifact))

reportRecoveryResiduals :: String -> [RecoveryFailure] -> IO ()
reportRecoveryResiduals _ [] = pure ()
reportRecoveryResiduals target failures =
  hPutStrLn stderr $ "  Prepared recovery residuals (" ++ target ++ "): "
    ++ show failures

-- | Turn mode (@--turn@): classify the raw
-- turn text (or accept a caller-supplied @--turn-verdict@), splice the
-- matching template, compile through the resident prepared-STG path, and write the rich
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
  lastAttempt <- newIORef Nothing
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
        -- scratch file's basename must match that header — 'runPipelineSessionSelected'
        -- looks up the compiled module by @capitalize (takeBaseName path)@
        -- (GhcPipeline.hs) exactly as 'tidepool_runtime::extract_module_name'
        -- does today for the existing two-spawn wrap_* templates
        -- (session.rs), which this mode's templates carry over unchanged.
        spliceInto :: FilePath -> IO (String, String, FilePath)
        spliceInto tmplFile = do
          tmplSrc <- readFile tmplFile
          let spliced = spliceTemplate tmplSrc turnSrc bindersStr
          if requestActivationPreview args
            then do
              let replace body = T.unpack (T.replace (T.pack "{{ACTIVATION_PREVIEW}}") (T.pack body) (T.pack spliced))
                  opaque = "(TidepoolScaffoldText.pack \"<opaque value>\\nUse the input type to select fields or apply sessionInput.\", False)"
              (_, _, checkPath) <- writeSpliced (replace opaque)
              checked <- compiler CheckedEnvironment Set.empty GeneralCompile
                (Just (scopeFromWorkerRequest args)) checkPath (requestIncludes args) (requestBuildProductsDir args)
              inputType <- maybe (fail "activation is missing its checked input type") (pure . stripMonadHead) (crResultType checked)
              rendered <- satisfiesCapturedConstraint (crHscEnv checked) (crTargetTcGblEnv checked)
                "__tidepoolActivationConstraint" inputType
              writeSpliced (replace (if rendered
                then "TidepoolInspection.workbenchActivationDisplay __activationBudget __activationInput"
                else opaque))
            else writeSpliced spliced
        writeSpliced spliced = do
          let modName = fromMaybe "Input" (extractModuleName spliced)
          createDirectoryIfMissing True outDir
          let modulePath = outDir </> modName ++ ".hs"
          writeFile modulePath spliced
          -- Retain the exact attempted source, but write the diagnostic copy
          -- only on failure. Successful TurnOut already contains this source.
          writeIORef lastAttempt (Just (outDir </> "turn-attempt.hs", spliced))
          return (spliced, modName, modulePath)
    if requestActivationPreview args && (sbKind sb /= KBind || length (sbBinders sb) /= 1)
      then fail "activation requires exactly one generated input binder"
      else pure ()
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
        items <- timePhase timing "declaration_binders" $ extractBindersNamed modulePath (requestIncludes args) modName
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
            -- A prepared turn uses one compiler pass for the checked metadata
            -- and the prepared modules.
            compileTurn modulePath = do
              prepared <- compiler PreparedStg (Map.keysSet (requestRetainedGenerations args)) GeneralCompile (Just scope) modulePath (requestIncludes args) (requestBuildProductsDir args)
              return (pprPipelineResult prepared, pprModules prepared, pprDependencies prepared)
            compileVariants _ [] = error ("--turn: no --turn-template for kind " ++ templateSelectorWireName selector)
            compileVariants index (tmplFile:rest) = do
              (spliced, _modName, modulePath) <- spliceInto tmplFile
              attempted <- try (compileTurn modulePath)
              case attempted of
                Right (result, preparedModules, dependencies) ->
                  return (index, spliced, modulePath, result, preparedModules, dependencies)
                Left err@(_ :: SomeException) -> case (fromException err :: Maybe SourceError, rest) of
                  (Just _, _ : _) -> compileVariants (index + 1) rest
                  _               -> throwIO err
        if requestActivationPreview args
            && (selector /= SBind || length (sbBinders sb) /= 1 || length matching /= 1)
          then fail "activation requires one prepared bind template"
          else pure ()
        (variant, spliced, compiledPath, result, preparedModules, dependencies) <- compileVariants (0 :: Int) matching
        let binds       = prBinds result
            hscEnv      = prHscEnv result
            mCapturedTy = fmap T.pack (prCapturedType result)
            warnTexts   = map T.pack (prWarnings result)
        -- Projection remains outside compileVariants. Its entry is the settled
        -- scaffold, and its constructors join the shared metadata before write.
        preparedArtifacts <- prepareArtifacts caches compiledPath hscEnv preparedModules
          [preparedScaffoldTargetName] (standardAuxiliaryRoots binds) (requestRetainedGenerations args)
        let asksSites = concatMap paYieldSites preparedArtifacts
        timePhase timing "prepared_sidecars" $ writePreparedSidecars InlineYieldSites outDir binds (prTyCons result) mCapturedTy warnTexts preparedArtifacts
        timePhase timing "prepared_write" $ writePreparedArtifacts outDir preparedArtifacts
        -- Mutable turns never enter the artifact cache, but publication must
        -- still reject source changes observed during this compilation.
        validateDependencyEvidence dependencies
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
  case res of
    Left _ -> do
      attempted <- readIORef lastAttempt
      forM_ attempted $ \(output, source) -> do
        _ <- try (writeFile output source) :: IO (Either IOException ())
        pure ()
    Right _ -> pure ()
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
  provisionalOutput <- newIORef Nothing
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
      -- Preserve the latest plan for failure diagnostics without encoding and
      -- writing a provisional result before every successful check attempt.
      writeIORef provisionalOutput (Just (out, plan, rendered))
      compiler CheckedEnvironment Set.empty GeneralCompile scope modulePath (requestIncludes args) (requestBuildProductsDir args)) initialPlan
    checkedSource <- either fail pure (renderCellCheckSource template analyzed)
    (finalPlan, finalSource, compiled) <- if null (cellPlanDisplayTargets analyzed)
      then pure (analyzed, checkedSource, provisional)
      else do
        contextDeclarations <- cellDisplayDeclarations DisplayInstanceContexts provisional analyzed
        let contextual = installCellDisplayDeclarations contextDeclarations analyzed
        contextualSource <- either fail pure (renderCellCheckSource template contextual)
        writeFile modulePath contextualSource
        contextChecked <- compiler CheckedEnvironment Set.empty GeneralCompile scope modulePath (requestIncludes args) (requestBuildProductsDir args)
        declarations <- cellDisplayDeclarations DisplayInstanceFields contextChecked analyzed
        let finalized = installCellDisplayDeclarations declarations analyzed
        finalizedSource <- either fail pure (renderCellCheckSource template finalized)
        writeFile modulePath finalizedSource
        finalizedResult <- compiler CheckedEnvironment Set.empty GeneralCompile scope modulePath (requestIncludes args) (requestBuildProductsDir args)
        pure (finalized, finalizedSource, finalizedResult)
    -- Statement preparation checks these rendered pins in their actual value
    -- modules before any declaration commits or effect runs.
    expressionPlans <- cellExpressionPlans compiled
    BS.writeFile out
      (encodeCellOut finalPlan (crCheckedBinderPins compiled) expressionPlans finalSource)
  case res of
    Left _ -> do
      provisional <- readIORef provisionalOutput
      forM_ provisional $ \(out, plan, rendered) -> do
        _ <- try (BS.writeFile out (encodeCellOut plan [] [] rendered)) :: IO (Either IOException ())
        pure ()
    Right _ -> pure ()
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
