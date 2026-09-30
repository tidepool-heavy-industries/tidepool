{-# LANGUAGE RankNTypes #-}

module Main where

import System.Environment (getArgs)
import System.FilePath (takeBaseName, takeDirectory, takeFileName, (</>))
import System.Directory (createDirectoryIfMissing, removeFile, setCurrentDirectory)
import qualified Data.ByteString as BS
import Codec.CBOR.Encoding (encodeBytes, encodeListLen, encodeString, encodeWord)
import Codec.CBOR.Write (toStrictByteString)
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import Control.Exception
  ( evaluate, try, finally, throwIO, SomeAsyncException, SomeException, Exception
  , fromException, toException, IOException )
import Data.IORef (IORef, newIORef, readIORef, writeIORef)
import Data.List (intercalate, nub)
import Data.Maybe (fromMaybe, mapMaybe, isJust)
import Data.Word (Word64)
import Control.Monad (foldM, forM, forM_, when, unless)
import System.Exit (ExitCode(..), exitWith)
import System.IO (hClose, hPutStrLn, openBinaryTempFile, stderr, stdin, stdout, hSetBinaryMode, hSetEncoding, utf8)
import qualified System.Info as SystemInfo

import GHC.Types.SourceError (SourceError)
import GHC (Module, ModuleName, moduleName, moduleNameString, moduleUnit)
import GHC.Driver.Env (HscEnv, hsc_dflags)
import GHC.Driver.Session (targetProfile)
import GHC.Iface.Binary (CompressionIFace(..), TraceBinIFace(..), writeBinIface)
import GHC.Unit.Module.ModIface (ModIface)
import GHC.Unit.Types (unitString)
import GHC.Core (Bind(..), CoreBind)
import GHC.Core.DataCon (DataCon)
import GHC.Core.TyCon (TyCon)
import GHC.Types.Name (nameOccName)
import GHC.Types.Id (idName)
import GHC.Types.Name.Occurrence (occNameString)
import qualified Data.Text as T
import qualified Data.Text.Encoding as TE

import Tidepool.Binders
  ( extractBindersNamed
  , extractStmtBinders, classifyBlock, exportItemName
  , analyzeCell, renderCellCheckSource, CellSplitError(..), CellSourceSpan(..)
  , CellSourcePlan(..), CellAnalysisItem(..), installCellDisplayDeclarations
  , declarationSourceWithTemplate, renderDeclarationForTemplate
  , TurnKind(..), parseTurnKind
  , TemplateSelector(..), templateSelectorForVerdict, templateSelectorWireName
  , StmtBinders(..), TurnOut(..), renderAskJson, renderVerdictsJson
  , CheckedBinderPin(..) )
import Tidepool.GhcPipeline
  ( PipelineSelection(..), PreparedPipelineResult(..), CheckedEnvironmentResult(..)
  , runPipelineSessionSelected, CompilePurpose(..), PipelineResult(..)
  , withResidentPipelineSelectedRequests, withExactInterfaceTransaction
  , CellDisplayPass(..), cellDisplayDeclarations, checkCellInstances
  , cellExpressionEvidence, cellCheckedBinderSignatures
  , satisfiesCapturedConstraint, stripMonadHead )
import Tidepool.ExecutionEncode (encodeWireProgram, encodeModuleProducts)
import Tidepool.ExecutionProjection (ProjectionContext(..), ProjectionError(..), prepareProjectionWithReachability, projectSelected, projectPreparedModuleGroups, preparedRootIdentity, resolveTextPackageUnit)
import Tidepool.PreparedFormatting (resolveFormattingAuthority)
import Tidepool.PreparedTime (resolveTimeAuthority)
import Tidepool.PreparedJson (resolveJsonAuthority)
import Tidepool.ExecutionSchema
  ( Architecture(..), Endianness(..), SymbolIdentity(..), TargetDescriptor(..)
  , WireProgram(..), ProjectedGroup(..), SiteRow(..) )
import qualified Tidepool.ExecutionSchema as Execution
import qualified Tidepool.EffectSchema
import Tidepool.PreparedStg
  ( PreparedModule(..), PreparedBodyCache, newPreparedBodyCache
  , evictPreparedBodyMatching )
import Tidepool.PreparedRecovery
  ( RecoveryFailure, RecoveredClosure(..), newPreparedRecoveryWithPackageRoots )
import Tidepool.ModuleCandidates (ModuleCandidate(..))
import Tidepool.CertifiedProducts (encodeCertifiedProducts, resolvePackageGlobal)
import Tidepool.OriginalProductRoots (requiredOriginalPackageGlobalsWithExact)
import Tidepool.ExactHydration (ExactIfaceArtifact(..))
import Tidepool.PackageWitness (PackageImportRoot, encodePackageImports)
import qualified Crypto.Hash.SHA256 as SHA256
import Numeric (showHex)
import Tidepool.DeclarationJoin
  ( DeclarationOperation(..), readDeclarationOperation, validateDeclarationJoin
  , renderDeclarationJoinOutcome, inspectDeclarationArtifacts
  , renderDeclarationInventoryOutcome )
import qualified Tidepool.WorkerServer as WorkerServer
import Tidepool.DiagJson
  ( ReportOutcome(..), DiagSeverity(..), Diag(..), SourceRejection(..)
  , diagsFromSourceError, diagFromException, renderDiagsJson )
import Tidepool.ExtractUtil (capitalize)
import Tidepool.ExtractRequest (InspectionRequest(..), WorkerRequest(..), workerRequestFromArgv, workerRequestFlag)
import Tidepool.Introspection (InspectionResult(..), encodeInspectionResults, runInspection)
import Tidepool.ExactScope
  ( ExactCompilation(..), ExactScope(..), ExactProduct(..), ExactOriginalGroup(..)
  , CheckedCellAdmission(..), readExactScope, revalidateExactScope, writeExactCompilation )
import Tidepool.CheckedCell (encodeCheckedSignature)
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
  ( DependencyEvidence(..), DependencyModule(..), ProductAvailability(..)
  , renderDependencyEvidence, revalidateDependencyEvidence )

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
-- @Tidepool.Session.*@ module. These caches carry no incarnation, so unlike
-- 'Tidepool.GhcPipeline.sanitizeMemo' they drop every session module.
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
  CellDanglingOperatorFailure sourceSpan operatorText ->
    throwIO (LocatedCellRejection sourceSpan
      ("cell ends with a dangling operator `" ++ operatorText
        ++ "`: remove it or supply its right operand"))
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
dispatch compiler caches timing args = do
  admitted <- trySynchronous $ forM_ (requestSessionArtifacts args) $ \manifest -> do
    scope <- readExactScope manifest >>= either fail pure
    forM_ (scopeCheckedCell scope) $ \_ ->
      unless (requestCell args && not (requestTurn args) && not (requestClassify args)
          && null (requestInspections args) && not (isJust (requestDeclarationJoin args))
          && not (requestCertifyHomeProducts args) && not (requestActivationPreview args))
        (fail "checked-cell authorization requires its dedicated cell-check request")
  case admitted of
    Left failure -> reportDiags (Left failure)
    Right () -> case requestDeclarationJoin args of
      Just manifest -> runDeclarationOperation args manifest
      Nothing -> dispatchSource compiler caches timing args

dispatchSource :: Compiler -> RecoveryCaches -> Bool -> WorkerRequest -> IO ExitCode
dispatchSource compiler caches timing args =
  case requestFiles args of
    [] -> reportDiags (Left (toException (userError "worker request contains no input")))
    (file : _)
        -- Classification consumes every input; all other modes use the first.
        | isJust (requestInspectTypeBatch args)
          && not (length (requestInspections args) > 1 && all isInspectionTypeQuery (requestInspections args))
                                                  -> reportDiags (Left (toException (userError "inspection type batch requires at least two type queries and no other query kinds")))
        | requestActivationPreview args && (not (requestTurn args) || not (isJust (requestTurnVerdict args)))
                                                  -> reportDiags (Left (toException (userError "activation requires a prepared turn with a generated bind verdict")))
        | requestCell args                        -> runCellMode compiler caches args file
        | requestClassify args                    -> runClassifyMode timing args
        | not (null (requestInspections args))    -> runInspectionMode compiler args file
        -- A turn may also carry session fields, so it precedes session dispatch.
        | requestTurn args                        -> runTurnMode compiler caches args file
        -- Multi-target compilation may also carry a stable-value scope.
        | not (null (requestTargets args))        -> timePhase timing "total" (processFile compiler caches timing args file)
        -- Normal one-shot extraction.
        | otherwise                           -> timePhase timing "total" (processFile compiler caches timing args file)

runDeclarationOperation :: WorkerRequest -> FilePath -> IO ExitCode
runDeclarationOperation args manifest = do
  result <- trySynchronous $ do
    out <- maybe (fail "declaration operation requires an output path") pure
      (requestDeclarationJoinOut args)
    operation <- readDeclarationOperation manifest
    rendered <- withExactInterfaceTransaction (requestIncludes args) $ \env ->
      case operation of
        InspectInventory artifacts -> renderDeclarationInventoryOutcome
          <$> inspectDeclarationArtifacts env artifacts
        ValidateJoin input -> renderDeclarationJoinOutcome
          <$> validateDeclarationJoin env input
    writeFile out rendered
  reportDiags result

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
reportDiags = reportDiagsWithWarnings . fmap (const [])

reportDiagsWithWarnings :: Either SomeException [Diag] -> IO ExitCode
reportDiagsWithWarnings (Left e) = do
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
reportDiagsWithWarnings (Right warnings) =
  putStrLn (renderDiagsJson ReportSuccess warnings) >> pure ExitSuccess

-- | Whether a generic extraction needs stable session values in scope.
hasSessionScope :: WorkerRequest -> Bool
hasSessionScope args = not (null (requestInjectVals args))
  || isJust (requestSessionRoot args) || isJust (requestSessionArtifacts args)

-- | Project the session portion of a worker request. Callers decide whether
-- the resulting scope is active.
scopeFromWorkerRequest :: WorkerRequest -> SessionScope
scopeFromWorkerRequest args = SessionScope
  { ssRoot      = fromMaybe "" (requestSessionRoot args)
  , ssValIfaces = mapMaybe parseValModule (requestInjectVals args)
  , ssExactScope = requestSessionArtifacts args
  , ssIncarnation = requestSessionIncarnation args
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
    let purpose = if requestCertifyHomeProducts args
          then CertifyHomeProductsCompile else GeneralCompile
    prepared <- compiler (PreparedProducts (requestModuleCandidates args)) (Map.keysSet (requestRetainedGenerations args)) purpose scope path (requestIncludes args) (requestBuildProductsDir args)
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
    (preparedArtifacts, productContext) <- prepareArtifacts caches path hscEnv (pprModules prepared) preparedTargets
      (standardAuxiliaryRoots binds) (requestRetainedGenerations args) (pprAcceptedCandidates prepared) (compilationScope <$> pprExactCompilation prepared)
    if null preparedArtifacts
      then ioError (userError "prepared extraction requires --target or --targets")
      else timePhase timing "prepared_sidecars" $ writePreparedSidecars SeparateYieldSites outDir binds tycons mCapturedTy warnTexts preparedArtifacts

    timePhase timing "prepared_write" $ writePreparedArtifacts outDir preparedArtifacts
    timePhase timing "module_products" $
      writeCertifiedProducts outDir hscEnv prepared productContext preparedArtifacts

  reportDiags res

writeCertifiedProducts
  :: FilePath -> HscEnv -> PreparedPipelineResult -> Maybe ProjectionContext
  -> [PreparedArtifact] -> IO ()
writeCertifiedProducts outDir hscEnv prepared productContext preparedArtifacts = do
    (availability, freshProducts) <- writeModuleProducts outDir hscEnv
      productContext (pprModules prepared) (pprProductInterfaces prepared)
      (pprPackageRoots prepared)
    let dependencies = pprDependencies prepared
        withCertified = foldr (\candidate -> Map.insert
          (candidateUnit candidate, candidateModule candidate) ProductReady)
          availability (pprAcceptedCandidates prepared)
        withAvailability node = node
          { dependencyModuleProduct = if dependencyModuleBoot node
              then ProductBoot
              else Map.findWithDefault (dependencyModuleProduct node)
                (dependencyModuleUnit node, dependencyModuleName node) withCertified
          }
        freshDependencies = dependencies
          { dependencyModules = map withAvailability (dependencyModules dependencies) }
        finalDependencies = case pprExactCompilation prepared of
          Nothing -> freshDependencies
          Just _ -> freshDependencies
            { dependencyCacheSafe = False, dependencySelectionComplete = False }
    writeDependencyEvidence outDir finalDependencies
    forM_ (pprExactCompilation prepared) $ \compilation -> do
      verified <- revalidateExactScope hscEnv (compilationScope compilation)
      either (ioError . userError) pure verified
      writeExactCompilation compilation freshDependencies
    productBytes <- BS.readFile (outDir </> "module-products.cbor")
    evidenceBytes <- BS.readFile (outDir </> "dependencies.json")
    certified <- encodeCertifiedProducts hscEnv (pprAcceptedCandidates prepared)
      (compilationScope <$> pprExactCompilation prepared)
      freshProducts [(paTarget artifact, paProgram artifact) | artifact <- preparedArtifacts]
      finalDependencies productBytes evidenceBytes
    case certified of
      Right bytes -> BS.writeFile (outDir </> "certified-products.cbor") bytes
      Left reason -> do
        hPutStrLn stderr ("product certification unavailable: " ++ reason)
        BS.writeFile (outDir </> "certified-products.cbor") BS.empty

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
  , paProgram :: WireProgram
  , paBytes :: BS.ByteString
  , paConstructors :: [DataCon]
  , paYieldSites :: [Tidepool.EffectSchema.YieldSite]
  }

-- Project before writing artifacts so the shared constructor
-- table includes exactly the GHC constructors admitted by prepared execution.
prepareArtifacts :: RecoveryCaches -> FilePath -> HscEnv -> [PreparedModule] -> [String] -> [String]
  -> Map.Map SymbolIdentity Word64 -> [ModuleCandidate] -> Maybe ExactScope
  -> IO ([PreparedArtifact], Maybe ProjectionContext)
prepareArtifacts _ _ _ _ [] _ _ _ _ = pure ([], Nothing)
prepareArtifacts caches input hscEnv modules targets@(firstTarget : _) auxiliaryRoots retainedGenerations candidates exactScope = do
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
  let exactProducts = maybe [] scopeProducts exactScope
      certifiedHomes = Set.fromList
        ([(candidateUnit candidate, candidateModule candidate) | candidate <- candidates]
         ++ [(originalUnit originalProduct, originalModule originalProduct) | originalProduct <- exactProducts])
      exactOriginals =
        [(originalUnit originalProduct, originalModule originalProduct,
          [(originalOrdinal group, originalBinders group, originalGlobals group)
           | group <- originalGroups originalProduct]) | originalProduct <- exactProducts]
      originalProducts =
        [(unitString (moduleUnit (pmModule prepared)), moduleNameString (moduleName (pmModule prepared)),
          either (Left . show) Right (projectPreparedModuleGroups (contextFor firstTarget) prepared))
        | prepared <- modules]
      recovery roots = newPreparedRecoveryWithPackageRoots hscEnv (rcFatIface caches) (rcOwnerIface caches)
        (rcPreparedBodies caches) certifiedHomes (contextFor firstTarget) modules roots
  recover <- recovery []
  artifacts <- forM targets $ \target -> do
    let context = contextFor target
    -- Package roots grow only from the finite exact original-group inventory.
    -- Recovered package code may expose another original group; rescan each
    -- projected target before admitting the final executable closure.
    initial <- timePhase timing "prepared_recover"
      (recover (projectionEntry context))
    let closePackages roots recovered = do
          let finalContext = context
                { projectionAuxiliaryRoots = projectionAuxiliaryRoots context ++ roots }
          selected <- timePhase timing "prepared_project" $
            requireProjection (prepareProjectionWithReachability finalContext
              (closureModules recovered) (closureReachability recovered))
          (program, constructors) <- requireProjection (projectSelected selected)
          required <- either (ioError . userError) pure
            (requiredOriginalPackageGlobalsWithExact originalProducts candidates exactOriginals (programGlobals program))
          let nextRoots = Set.toAscList (Set.fromList (roots ++ required))
          if nextRoots == roots
            then pure (recovered, program, constructors, roots)
            else do
              packageRoots <- forM nextRoots $ \identity -> do
                (identifier, _) <- resolvePackageGlobal hscEnv identity >>= either (ioError . userError) pure
                when (preparedRootIdentity identifier /= identity) $
                  ioError (userError "package recovery root differs from canonical original global")
                pure identifier
              withPackages <- recovery packageRoots
              next <- timePhase timing "prepared_recover_original_packages"
                (withPackages (projectionEntry context))
              closePackages nextRoots next
    (recovered, program, constructors, roots) <- closePackages [] initial
    reportRecoveryResiduals target (closureFailures recovered)
    let defined = Set.fromList
          [identity | group <- programBindings program
          , Execution.TopBinding identity _ <- case group of
              Execution.NonRecursive binding -> [binding]
              Execution.Recursive bindings -> bindings]
    when (any (`Set.notMember` defined) roots) $
      ioError (userError "required original-group package root has no executable definition")
    bytes <- timePhase timing "prepared_encode" $ evaluate (encodeWireProgram program)
    let admitted = Set.fromList (map siteId (programSites program))
        yieldSites =
          [ site
          | preparedModule' <- closureModules recovered
          , site <- pmYieldSites preparedModule'
          , Tidepool.EffectSchema.ysSite site `Set.member` admitted
          ]
    pure (PreparedArtifact target program bytes constructors yieldSites)
  pure (artifacts, Just (contextFor firstTarget))

-- A failed unrelated group is an explicit product miss, never a newly fatal
-- target compile. A complete product pairs every admitted group with the
-- skinny interface emitted by the same GHC transaction.
writeModuleProducts :: FilePath -> HscEnv -> Maybe ProjectionContext
  -> [PreparedModule] -> Map.Map ModuleName ModIface
  -> Map.Map ModuleName [PackageImportRoot]
  -> IO (Map.Map (String, String) ProductAvailability,
         [(T.Text, T.Text, BS.ByteString, [ProjectedGroup])])
writeModuleProducts _ _ Nothing _ _ _ = pure (Map.empty, [])
writeModuleProducts outDir hscEnv (Just context) modules interfaces packageRoots = do
  outcomes <- forM modules $ \prepared -> do
    let name = moduleName (pmModule prepared)
        key = (unitString (moduleUnit (pmModule prepared)), moduleNameString name)
    case Map.lookup name interfaces of
      Nothing -> do
        hPutStrLn stderr ("module product unavailable: no interface for " ++ moduleNameString name)
        pure (key, ProductMissingInterface, Nothing, Nothing)
      Just interface -> case projectPreparedModuleGroups context prepared of
        Left reason -> do
          hPutStrLn stderr ("module product unavailable: " ++ moduleNameString name
            ++ ": " ++ show reason)
          pure (key, ProductProjectionRejected, Nothing, Nothing)
        Right groups -> do
          (path, handle) <- openBinaryTempFile outDir "module-product.hi"
          hClose handle
          bytes <- (do
            writeBinIface (targetProfile (hsc_dflags hscEnv)) QuietBinIFace
              NormalCompression path interface
            BS.readFile path) `finally` removeFile path
          roots <- case Map.lookup name packageRoots of
            Nothing -> ioError (userError
              ("resolved direct package import inventory missing for " ++ moduleNameString name))
            Just selected -> pure selected
          let iface = ExactIfaceArtifact (fst key) (snd key) ""
                (shaHex bytes) []
              sidecar = encodePackageImports iface roots
          when (BS.length sidecar > 4 * 1024 * 1024) $
            ioError (userError "direct package import witness exceeds four MiB")
          pure (key, ProductReady, Just (T.pack (fst key),
            T.pack (snd key), bytes, groups), Just sidecar)
  let products = [moduleProduct | (_, _, Just moduleProduct, _) <- outcomes]
      packageBundles =
        [(unit, moduleName', sidecar)
        | ((unit, moduleName'), _, Just _, Just sidecar) <- outcomes]
  BS.writeFile (outDir </> "module-products.cbor") (encodeModuleProducts products)
  BS.writeFile (outDir </> "module-package-imports.cbor")
    (toStrictByteString (encodeListLen 3
      <> encodeString (T.pack "TPPKGBUNDLES") <> encodeWord 1
      <> encodeListLen (fromIntegral (length packageBundles))
      <> foldMap (\(unit, moduleName', sidecar) -> encodeListLen 3
        <> encodeString (T.pack unit) <> encodeString (T.pack moduleName')
        <> encodeBytes sidecar) packageBundles))
  pure (Map.fromList [(key, status) | (key, status, _, _) <- outcomes], products)

shaHex :: BS.ByteString -> String
shaHex = concatMap (\byte -> let text = showHex byte "" in
  replicate (2 - length text) '0' ++ text) . BS.unpack . SHA256.hash

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
        (_spliced, modName, modulePath) <- writeSplicedModule outDir lastAttempt spliced
        items <- timePhase timing "declaration_binders" $ extractBindersNamed modulePath (requestIncludes args) modName
        let binders = if null (sbBinders sb)
                        then map (T.pack . exportItemName) items
                        else map T.pack (sbBinders sb)
        return (TDecl binders items declarationSource)
      _kind -> compileClassifiedTurn compiler caches args timing outDir turnSrc sb bindersStr [] lastAttempt
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

-- | Write @spliced@ (a fully-rendered module source) to a scratch file under
-- 'outDir', named for its own @module X where@ header, and retain it in
-- 'lastAttempt' for on-failure diagnostics. Shared by 'runTurnMode' (the
-- @decl@ verdict, directly) and 'compileClassifiedTurn' (every other
-- verdict, via its own @spliceInto@).
writeSplicedModule
  :: FilePath -> IORef (Maybe (FilePath, String)) -> String
  -> IO (String, String, FilePath)
writeSplicedModule outDir lastAttempt spliced = do
  let modName = fromMaybe "Input" (extractModuleName spliced)
  createDirectoryIfMissing True outDir
  let modulePath = outDir </> modName ++ ".hs"
  writeFile modulePath spliced
  -- Retain the exact attempted source, but write the diagnostic copy
  -- only on failure. Successful TurnOut already contains this source.
  writeIORef lastAttempt (Just (outDir </> "turn-attempt.hs", spliced))
  return (spliced, modName, modulePath)

-- | Compile one already-classified, non-@decl@ turn statement (bind,
-- bind-discard, or expr) through the resident prepared-STG path and return
-- its 'TurnOut'. This is the compile half of 'runTurnMode' for every verdict
-- but @decl@, factored out so 'runCellMode''s single-item fold can drive the
-- SAME compile from a verdict and pin it already has from the whole-cell
-- check — in the same worker invocation, with no separate classify/compile
-- spawn.
insertCheckedTypeImports :: [String] -> String -> IO String
insertCheckedTypeImports [] source = pure source
insertCheckedTypeImports modules source =
  let marker = T.pack "default (Int, Double, Text)\n"
      (before, after) = T.breakOn marker (T.pack source)
  in if T.null after
       then fail "cell fold: turn template has no import insertion point"
       else pure (T.unpack before ++ concatMap (\modu -> "import qualified " ++ modu ++ "\n") modules
         ++ T.unpack after)

compileClassifiedTurn
  :: Compiler -> RecoveryCaches -> WorkerRequest -> Bool -> FilePath
  -> String -> StmtBinders -> String -> [String] -> IORef (Maybe (FilePath, String))
  -> IO TurnOut
compileClassifiedTurn compiler caches args timing outDir turnSrc sb bindersStr typeImports lastAttempt = do
    let templates = requestTurnTemplates args
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
          tmplWithImports <- insertCheckedTypeImports typeImports tmplSrc
          let spliced = spliceTemplate tmplWithImports turnSrc bindersStr
          if requestActivationPreview args
            then do
              let replace body = T.unpack (T.replace (T.pack "{{ACTIVATION_PREVIEW}}") (T.pack body) (T.pack spliced))
                  opaque = "(TidepoolScaffoldText.pack \"<opaque value>\\nUse the input type to select fields or apply sessionInput.\", False)"
              (_, _, checkPath) <- writeSplicedModule outDir lastAttempt (replace opaque)
              checked <- compiler CheckedEnvironment Set.empty GeneralCompile
                (Just (scopeFromWorkerRequest args)) checkPath (requestIncludes args) (requestBuildProductsDir args)
              inputType <- maybe (fail "activation is missing its checked input type") (pure . stripMonadHead) (crResultType checked)
              rendered <- satisfiesCapturedConstraint (crHscEnv checked) (crTargetTcGblEnv checked)
                "__tidepoolActivationConstraint" inputType
              writeSplicedModule outDir lastAttempt (replace (if rendered
                then "TidepoolInspection.workbenchActivationDisplay __activationBudget __activationInput"
                else opaque))
            else writeSplicedModule outDir lastAttempt spliced
    -- Four-shape selection (protocol note, "the verdict space has four
    -- shapes, not three"): a bind that binds no name selects its own
    -- template kind and skips the session-bind artifacts entirely —
    -- 'templateSelectorForVerdict' mirrors Rust's
    -- 'TemplateSelector::for_verdict' exactly.
    let selector = templateSelectorForVerdict (sbKind sb) (sbBinders sb)
    let scope = scopeFromWorkerRequest args
        matching = [f | (name, f) <- templates, name == templateSelectorWireName selector]
        -- A prepared turn uses one compiler pass for the checked metadata
        -- and the prepared modules.
        compileTurn modulePath = do
          compiler (PreparedProducts (requestModuleCandidates args))
            (Map.keysSet (requestRetainedGenerations args)) GeneralCompile
            (Just scope) modulePath (requestIncludes args)
            (requestBuildProductsDir args)
        compileVariants _ [] = error ("--turn: no --turn-template for kind " ++ templateSelectorWireName selector)
        compileVariants index (tmplFile:rest) = do
          (spliced, _modName, modulePath) <- spliceInto tmplFile
          attempted <- try (compileTurn modulePath)
          case attempted of
            Right prepared ->
              return (index, spliced, modulePath, prepared)
            Left err@(_ :: SomeException) -> case (fromException err :: Maybe SourceError, rest) of
              (Just _, _ : _) -> compileVariants (index + 1) rest
              _               -> throwIO err
    if requestActivationPreview args
        && (selector /= SBind || length (sbBinders sb) /= 1 || length matching /= 1)
      then fail "activation requires one prepared bind template"
      else pure ()
    (variant, spliced, compiledPath, prepared) <- compileVariants (0 :: Int) matching
    let result      = pprPipelineResult prepared
        preparedModules = pprModules prepared
        binds       = prBinds result
        hscEnv      = prHscEnv result
        mCapturedTy = fmap T.pack (prCapturedType result)
        warnTexts   = map T.pack (prWarnings result)
    -- Projection remains outside compileVariants. Its entry is the settled
    -- scaffold, and its constructors join the shared metadata before write.
    (preparedArtifacts, productContext) <- prepareArtifacts caches compiledPath hscEnv preparedModules
      [preparedScaffoldTargetName] (standardAuxiliaryRoots binds)
      (requestRetainedGenerations args) (pprAcceptedCandidates prepared) (compilationScope <$> pprExactCompilation prepared)
    let asksSites = concatMap paYieldSites preparedArtifacts
    timePhase timing "prepared_sidecars" $ writePreparedSidecars InlineYieldSites outDir binds (prTyCons result) mCapturedTy warnTexts preparedArtifacts
    timePhase timing "prepared_write" $ writePreparedArtifacts outDir preparedArtifacts
    timePhase timing "module_products" $
      writeCertifiedProducts outDir hscEnv prepared productContext preparedArtifacts
    -- Mutable turns never enter the artifact cache, but publication must
    -- still reject source changes observed during this compilation.
    validateDependencyEvidence (pprDependencies prepared)
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
runCellMode :: Compiler -> RecoveryCaches -> WorkerRequest -> FilePath -> IO ExitCode
runCellMode compiler caches args cellPath = do
  timing <- readTimingEnabled
  provisionalOutput <- newIORef Nothing
  res <- try $ do
    cellSource <- readFile cellPath
    templatePath <- requireArg "--cell-template" (requestCellTemplate args)
    template <- readFile templatePath
    admittedScope <- traverse (\manifest -> readExactScope manifest >>= either fail pure)
      (requestSessionArtifacts args)
    forM_ admittedScope $ \scope -> forM_ (scopeCheckedCell scope) $ \admission ->
      validateCheckedCellAdmission args admission cellSource template
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
    expressionEvidence <- cellExpressionEvidence compiled
    let outputBytes = encodeCellOut finalPlan (crCheckedBinderPins compiled)
          (map fst expressionEvidence) finalSource
    BS.writeFile out outputBytes
    forM_ admittedScope $ \scope -> forM_ (scopeCheckedCell scope) $ \admission -> do
      binderSignatures <- cellCheckedBinderSignatures compiled
      cellNow <- readFile cellPath
      templateNow <- readFile templatePath
      validateCheckedCellAdmission args admission cellNow templateNow
      verified <- revalidateExactScope (crHscEnv compiled) scope
      either fail pure verified
      let text = encodeString . T.pack
          signatures = binderSignatures ++ map snd expressionEvidence
          receipt = encodeListLen 9
            <> text "TPEXACTCHECK" <> text "1" <> text (scopeRequestSha256 scope)
            <> text (checkedAdmissionDigest admission) <> text (checkedCellSha256 admission)
            <> text (checkedTemplateSha256 admission) <> text (shaHex outputBytes)
            <> text (shaHex (TE.encodeUtf8 (T.pack finalSource)))
            <> encodeListLen (fromIntegral (length signatures))
            <> foldMap encodeCheckedSignature signatures
      BS.writeFile (outDir </> "checked-cell.cbor") (toStrictByteString receipt)
    -- Best-effort, and entirely inside this SAME 'try': a fold failure (an
    -- ineligible cell shape, a missing template, a real compile rejection)
    -- must never turn a SUCCESSFUL whole-cell check into a reported failure.
    -- 'attemptCellFoldTurn' catches its own exceptions and simply leaves
    -- '--turn-out' unwritten, which is the caller's documented signal to
    -- fall back to its own separate '--turn' request.
    when (requestCellFoldTurn args) $
      attemptCellFoldTurn compiler caches args timing outDir finalPlan compiled
    pure (crWarnings compiled)
  case res of
    Left _ -> do
      provisional <- readIORef provisionalOutput
      forM_ provisional $ \(out, plan, rendered) -> do
        _ <- try (BS.writeFile out (encodeCellOut plan [] [] rendered)) :: IO (Either IOException ())
        pure ()
    Right _ -> pure ()
  reportDiagsWithWarnings res

validateCheckedCellAdmission :: WorkerRequest -> CheckedCellAdmission -> String -> String -> IO ()
validateCheckedCellAdmission args admission cellSource template = do
  templateDigests <- forM (requestTurnTemplates args) $ \(kind, path) -> do
    bytes <- BS.readFile path
    pure (kind, shaHex bytes)
  unless (shaHex (TE.encodeUtf8 (T.pack cellSource)) == checkedCellSha256 admission
      && shaHex (TE.encodeUtf8 (T.pack template)) == checkedTemplateSha256 admission
      && templateDigests == checkedTurnTemplates admission
      && requestInjectVals args == checkedInjectedModules admission)
    (fail "cell body, wrapper or injected interfaces differ from immutable admission")

-- | After a successful whole-cell check, attempt ONE further compile in the
-- SAME worker invocation — no separate spawn — when the cell resolved to
-- exactly one item, that item is a bind (@x \<- e@ / @let x = e@, never a
-- bare expression or a declaration), and the caller asked for the fold
-- ('requestCellFoldTurn'). The bind's binder types are already known from
-- this SAME check ('crCheckedBinderPins'): reusing them as an explicit
-- signature (mirroring the Rust runtime's 'run_turn_pinned') is what makes
-- this safe with only ONE typecheck of the statement ever happening, so
-- there is nothing for two independent generalizations to disagree about.
--
-- Deliberately narrower than every fold-eligible shape: an expression item
-- compiles through a Rust-built observation wrapper
-- (`compile_block_off_checkout`'s `assemble_observation_module`) whose
-- generated binder name must avoid every name already visible in scope —
-- resolving that collision-free name is Rust-side session state this
-- worker invocation does not have before the check runs, so an eligible
-- expression-only cell still falls back to the ordinary two-request path.
--
-- A synchronous failure emits a bounded diagnostic and leaves '--turn-out'
-- unwritten; this never touches the successful '--cell-out'. Asynchronous
-- cancellation propagates through the worker's existing exception boundary.
attemptCellFoldTurn
  :: Compiler -> RecoveryCaches -> WorkerRequest -> Bool -> FilePath
  -> CellSourcePlan -> CheckedEnvironmentResult -> IO ()
attemptCellFoldTurn compiler caches args timing outDir finalPlan compiled = do
  result <- trySynchronous attempt
  case result of
    Right () -> pure ()
    Left exception -> hPutStrLn stderr
      ("cell fold unavailable: " ++ take 8192 (show exception))
  where
    attempt = case soleBindItem finalPlan of
      Nothing -> pure ()
      Just item -> do
        outFile <- requireArg "--turn-out" (requestTurnOut args)
        let sb = cellAnalysisVerdict item
            turnSrc = cellAnalysisSource item
            pins = itemBinderPins 0 (sbBinders sb) (crCheckedBinderPins compiled)
        bindersStr <- either fail pure (renderPinnedBinders (sbBinders sb) pins)
        lastAttempt <- newIORef Nothing
        let typeImports = nub (concatMap checkedPinImports [pin | Just pin <- pins])
        turnOut <- compileClassifiedTurn compiler caches args timing outDir turnSrc sb bindersStr typeImports lastAttempt
        BS.writeFile outFile (encodeTurnOut turnOut)

-- | The cell's sole item, when the whole-cell check resolved to exactly one
-- item total (so index 0 both in 'cellPlanItems' and in every pin key
-- 'renderCellCheckSource' generated for it) and that item is a bind. A
-- declaration always stages through its own candidate-module path
-- (Rust's @finalize_cell_install@); an expression needs the Rust-built
-- observation wrapper this worker invocation does not have (see
-- 'attemptCellFoldTurn'). Neither is attempted here.
soleBindItem :: CellSourcePlan -> Maybe CellAnalysisItem
soleBindItem plan = case cellPlanItems plan of
  [item] | sbKind (cellAnalysisVerdict item) == KBind -> Just item
  _ -> Nothing

-- | Resolve one item's binder pins by exact key, mirroring the Rust
-- runtime's own @CellCheck::pins_for_item@: each binder's post-zonk type,
-- keyed @__tidepool_cell_pin_\<item\>_\<binder\>@ (see
-- 'Tidepool.Binders.renderCellCheckSource'), in the SAME order as
-- @binders@.
itemBinderPins :: Int -> [String] -> [CheckedBinderPin] -> [Maybe CheckedBinderPin]
itemBinderPins index binders pins =
  [ lookupPin (pinKey binder) | binder <- binders ]
  where
    pinKey binder = "__tidepool_cell_pin_" ++ show index ++ "_" ++ binder
    lookupPin key = case filter ((== key) . checkedPinKey) pins of
      [pin] -> Just pin
      _ -> Nothing

-- | Render the same @{{BINDERS}}@ substitution the Rust runtime's
-- @run_turn_pinned@ builds: @binder :: Type@ for one binder, @(b1, b2) ::
-- (T1, T2)@ for several — spliced into @pure ({{BINDERS}})@, an ordinary
-- Haskell type annotation that pins the compiled bind's generalization to
-- EXACTLY the type the whole-cell check already inferred. A bind with no
-- binders (a discarding bind, @_ \<- e@) has nothing to pin.
renderPinnedBinders :: [String] -> [Maybe CheckedBinderPin] -> Either String String
renderPinnedBinders [] _ = Right ""
renderPinnedBinders binders pins
  | any (== Nothing) pins =
      Left "cell fold: whole-cell check returned no type for a binder"
  | [binder] <- binders, [Just pin] <- pins = Right (binder ++ " :: " ++ checkedPinType pin)
  | otherwise = Right
      ( "(" ++ intercalate ", " binders ++ ") :: ("
      ++ intercalate ", " [ checkedPinType pin | Just pin <- pins ] ++ ")" )

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
