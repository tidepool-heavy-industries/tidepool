{-# LANGUAGE RankNTypes #-}

module Main where

import System.Environment (getArgs)
import System.FilePath (takeBaseName, takeDirectory, takeFileName, normalise, (</>))
import System.Directory (createDirectoryIfMissing, setCurrentDirectory, makeAbsolute, canonicalizePath, doesPathExist)
import qualified Data.ByteString as BS
import qualified Data.ByteString.Lazy as BL
import qualified Codec.CBOR.Decoding as CD
import Codec.CBOR.Read (deserialiseFromBytes)
import Codec.CBOR.Encoding (encodeBytes, encodeListLen, encodeString, encodeWord, encodeWord64, encodeNull)
import Codec.CBOR.Write (toStrictByteString)
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import Control.Exception
  ( evaluate, try, throwIO, SomeAsyncException, SomeException, Exception
  , fromException, toException, IOException )
import Data.IORef (IORef, newIORef, readIORef, writeIORef)
import Data.List (intercalate, nub, isInfixOf, isPrefixOf, stripPrefix)
import Data.Maybe (fromMaybe, mapMaybe, isJust)
import Data.Word (Word64)
import Data.Bits (shiftR)
import Control.Monad (replicateM, foldM, forM, forM_, when, unless, void)
import System.Exit (ExitCode(..), exitWith)
import System.IO (hPutStrLn, stderr, stdin, stdout, hSetBinaryMode, hSetEncoding, utf8)
import qualified System.Info as SystemInfo

import GHC.Types.SourceError (SourceError)
import GHC (Module, ModuleName, moduleName, moduleNameString, moduleUnit, mkModuleName)
import GHC.Driver.Env (HscEnv)
import GHC.Unit.Module.ModIface (ModIface, mi_module)
import GHC.Unit.Types (unitString)
import GHC.Core (Bind(..), CoreBind)
import GHC.Core.DataCon (DataCon)
import GHC.Core.TyCon (TyCon)
import GHC.Types.Name (nameOccName, nameModule)
import GHC.Builtin.Types (intTyConName)
import GHC.Types.Id (idName)
import GHC.Types.Name.Occurrence (occNameString, isSymOcc, mkVarOcc)
import qualified Data.Text as T
import qualified Data.Text.Encoding as TE

import Tidepool.Binders
  ( extractBindersNamed
  , classifyWithFlags, classifyBlock, exportItemName, defaultParserDynFlags
  , analyzeCell, analyzeOrderedCell, cellInferenceSegments, renderCellCheckSource, CellSplitError(..), CellSourceSpan(..)
  , CellSourcePlan(..), CellAnalysisItem(..), CellExpressionPlan(..), BoundBinder(..),
    SourcePrologue(..), LocatedPragma(..), LocatedImport(..), ExpressionLiftPlan(..), ExpressionPresentation(..), installCellDisplayDeclarations
  , declarationSourceWithTemplateFlags, renderDeclarationForTemplate
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
  , satisfiesCapturedConstraint, stripMonadHead, generatedScaffoldRecipe, activationPreviewInputType )
import Tidepool.ExecutionEncode
  ( encodeWireProgram, ModuleProductEncoding, prepareModuleProductEncoding
  , moduleProductInput, moduleProductBytes, encodeModuleProductInventory )
import Tidepool.ExecutionProjection (ProjectionContext(..), ProjectionError(..), prepareProjectionWithReachability, projectSelected, PreparedModuleProducts, projectOriginalHomeModuleProducts, preparedModuleProductOutcomes, preparedRootIdentity, resolveTextPackageUnit)
import Tidepool.PreparedFormatting (resolveFormattingAuthority)
import Tidepool.PreparedTime (resolveTimeAuthority)
import Tidepool.PreparedJson (resolveJsonAuthority)
import Tidepool.ExecutionSchema
  ( Architecture(..), Endianness(..), SymbolIdentity(..), TargetDescriptor(..)
  , WireProgram(..), SiteRow(..) )
import qualified Tidepool.ExecutionSchema as Execution
import qualified Tidepool.EffectSchema
import Tidepool.PreparedStg
  ( PreparedModule(..), PreparedBodyCache, newPreparedBodyCache
  , evictPreparedBodyMatching )
import Tidepool.PreparedRecovery
  ( RecoveryFailure, RecoveredClosure(..), newPreparedRecoveryWithPackageRoots
  , preparedRecoveryClosure, growPreparedRecovery )
import Tidepool.ModuleCandidates (ModuleCandidate(..), candidateExecutionSources)
import Tidepool.CompileInput (writeCompileInputProof)
import Tidepool.CertifiedProducts (encodeCertifiedProducts, resolvePackageGlobal)
import Tidepool.OriginalProductRoots (requiredOriginalPackageGlobalsWithRetained)
import Tidepool.ExactHydration (ExactIfaceArtifact(..), OriginalInterfaceArtifacts
  , newOriginalInterfaceArtifacts, originalInterfaceBytes)
import Tidepool.PackageWitness (PackageImportEvidence(..), PackageImportRoot(..), encodePackageImports)
import Tidepool.ExecutionSource
  ( ExecutionSourceRecipe(..), ExecutionSourceGraph(..), ExecutionSourceIdentity(..)
  , ExecutionSourceOwner(..), ExecutionSourceRef(..), ExecutionSourceFailure(..)
  , issueExecutionSourceRecipe, executionIdentityKey, executionSourceProspectiveReferences )
import qualified Crypto.Hash.SHA256 as SHA256
import Numeric (showHex, readHex)
import Tidepool.DeclarationJoin
  ( DeclarationOperation(..), readDeclarationOperation, validateDeclarationJoin
  , DeclarationExport(..), ExportIdentity(..), ExportNamespace(..)
  , renderDeclarationJoinOutcome, inspectDeclarationArtifacts
  , renderDeclarationInventoryOutcome )
import qualified Tidepool.WorkerServer as WorkerServer
import Tidepool.DiagJson
  ( ReportOutcome(..), DiagSeverity(..), Diag(..), SourceRejection(..), InputRejection(..), DependencyLoadFailure(..)
  , diagsFromSourceError, diagFromException, renderDiagsJson )
import Tidepool.ExtractUtil (capitalize)
import Tidepool.ExtractRequest (InspectionRequest(..), WorkerRequest(..), workerRequestFromArgv, workerRequestFlag)
import Tidepool.Introspection (InspectionResult(..), encodeInspectionResults, runInspection)
import Tidepool.ExactScope
  ( ExactCompilation(..), ExactScope(..), ExactProduct(..), ExactOriginalGroup(..)
  , originalGroupFromProjected, originalGroupFromCandidate
  , CheckedCellAdmission(..), CheckedItemAdmission(..), CheckedItemPurpose(..), CheckedDisplayAdmission(..), PlannedCellAdmission(..), PlannedCellSlot(..)
  , readExactScope, revalidateExactScope, writeExactCompilation, extendSourceSelectedOriginals, extendExactExecutionSources, extendExactExecutionSourcesWithinBudget )
import Tidepool.CheckedPrefixImports (CompletedValueImport(..))
import GHC.Core.Type (splitFunTy_maybe)
import Tidepool.CheckedCell (CheckedSignature(..), encodeCheckedSignature
  , captureCheckedTypeWitness, sealCheckedTypeWitness, encodeCheckedTypeWitness)
import Tidepool.PlannedDeclaration
  ( PlannedDeclaration, PlannedDeclarationRejection(..), PlannedDeclarationInventory, plannedExports, plannedSource, plannedCheckPlan, replaceTemplateModuleHeader
  , preparePlannedDeclaration, certifyPlannedDeclaration
  , renderPlannedDeclarationInventory, plannedInterfaceFingerprint )
import Tidepool.Session
  ( SessionScope(..), preparedScaffoldTargetName, preparedResumeTargetName
  , preparedApplyEntryTargetName, preparedApplyValueTargetName
  , parseSessionModule, sessionHiPath )
import Tidepool.FatIface
  ( FatIfaceCache, newFatIfaceCache, evictFatIfaceMatching
  , OwnerInterfaceCache, newOwnerInterfaceCache, evictOwnerInterfaceMatching )
import Tidepool.SessionArtifacts
  ( mkBoundBinders, parseValModule )
import Tidepool.Metadata
  ( collectDataCons, dcToMeta, mergeMetaPreserving, targetBindingHasIO
  , wiredInDataCons )
import Tidepool.CborEncode (encodeMetadata, encodeTurnOut, encodeCellOut)
import Tidepool.Timing (readTimingEnabled, timePhase, timeDetailPhase)
import Tidepool.TurnSource (extractModuleName, spliceTemplate, replaceTemplateMarker)
import Tidepool.DependencyEvidence
  ( DependencyEvidence(..), DependencyModule(..), DependencySource(..), DependencyImport(..), ProductAvailability(..)
  , renderDependencyEvidence, revalidateDependencyEvidence, selectedHomeRequirements )

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
  CellUnsupportedLocalFixity sourceSpan ->
    throwIO (LocatedCellRejection sourceSpan
      "local fixity cannot cross prepared item boundaries; put the operator and its fixity in an authored declaration group")
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
  args <- if requestHarnessProfile parsedWorkerRequest && not (requestCheckSource parsedWorkerRequest)
            then spliceHarnessProfilePragma parsedWorkerRequest
            else pure parsedWorkerRequest
  dispatch compiler caches timing args

-- | Dispatch one decoded worker request.
dispatch
  :: Compiler -> RecoveryCaches -> Bool -> WorkerRequest -> IO ExitCode
dispatch compiler caches timing args = do
  admitted <- trySynchronous $ do
    when (requestCheckSource args) $
      unless (length (requestFiles args) == 1 && not (requestCell args)
          && not (requestCellPlan args) && not (requestTurn args) && not (requestClassify args)
          && null (requestInspections args) && not (hasSessionScope args)
          && not (isJust (requestDeclarationJoin args)) && not (isJust (requestDeclarationJoinOut args))
          && not (requestCertifyHomeProducts args) && not (requestActivationPreview args)
          && not (requestHarnessProfile args) && not (requestCellFoldTurn args)
          && not (isJust (requestOutDir args)) && not (isJust (requestTarget args))
          && null (requestTargets args) && not (isJust (requestBindGen args))
          && null (requestTurnTemplates args) && not (isJust (requestTurnOut args))
          && not (isJust (requestTurnVerdict args)) && not (isJust (requestTurnPin args))
          && not (isJust (requestCellOut args)) && not (isJust (requestCellTemplate args))
          && not (isJust (requestClassifyOut args)) && not (isJust (requestInspectOut args))
          && not (isJust (requestInspectTypeBatch args)) && not (isJust (requestSessionArtifacts args))
          && not (isJust (requestSessionIncarnation args))
          && not (requestTargetModuleOnly args) && not (requestInspectionStrict args)
          && Map.null (requestRetainedGenerations args))
        (fail "source checking cannot carry product or notebook authority")
    when (requestCellPlan args) $
      unless (length (requestFiles args) == 1 && not (requestCell args)
          && not (requestTurn args) && not (requestClassify args)
          && null (requestInspections args) && not (isJust (requestSessionArtifacts args))
          && not (isJust (requestDeclarationJoin args)) && not (isJust (requestModuleCandidates args))
          && not (requestCertifyHomeProducts args) && not (requestActivationPreview args)
          && not (requestCellFoldTurn args) && not (isJust (requestBindGen args))
          && Map.null (requestRetainedGenerations args))
        (throwIO InvalidCellPlanRequest)
    forM_ (requestSessionArtifacts args) $ \manifest -> do
      scope <- readExactScope manifest >>= either fail pure
      forM_ (scopeIncludePaths scope) $ \includes ->
        unless (requestIncludes args == includes)
          (throwIO SearchInputsChanged)
      forM_ (scopeCheckedCell scope) $ \_ ->
        unless (requestCell args && not (requestTurn args) && not (requestClassify args)
          && null (requestInspections args) && not (isJust (requestDeclarationJoin args))
          && not (requestCertifyHomeProducts args) && not (requestActivationPreview args))
          (throwIO CheckedPurposeMismatch)
      forM_ (scopeCheckedItem scope) $ \admission -> do
        let previewPurpose = case itemPurpose admission of
              AuthoredCheckedItem -> not (requestActivationPreview args)
              HostActivationInput -> requestActivationPreview args
                && length (requestFiles args) == 1
                && not (requestCellPlan args) && not (requestCheckSource args)
                && not (requestHarnessProfile args) && not (requestCellFoldTurn args)
                && null (requestTargets args)
        unless (requestTurn args && not (requestCell args) && not (requestClassify args)
          && null (requestInspections args) && not (isJust (requestDeclarationJoin args))
          && not (requestCertifyHomeProducts args) && previewPurpose
          && not (isJust (requestTurnPin args)) && not (isJust (requestTarget args)))
          (throwIO CheckedPurposeMismatch)
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
        | requestCheckSource args                 -> runSourceCheckMode compiler args file
        | requestCellPlan args                    -> runCellPlanMode args file
        | requestCell args                        -> runCellMode compiler caches args file
        | requestClassify args                    -> runClassifyMode timing args
        | not (null (requestInspections args))    -> runInspectionMode compiler args file
        -- A turn may also carry session fields, so it precedes session dispatch.
        | requestTurn args                        -> runTurnMode compiler caches args file
        -- Multi-target compilation may also carry a stable-value scope.
        | not (null (requestTargets args))        -> timePhase timing "total" (processFile compiler caches timing args file)
        -- Normal one-shot extraction.
        | otherwise                           -> timePhase timing "total" (processFile compiler caches timing args file)

runSourceCheckMode :: Compiler -> WorkerRequest -> FilePath -> IO ExitCode
runSourceCheckMode compiler args path = do
  checked <- trySynchronous $ do
    compiled <- compiler (maybe CheckedEnvironment CheckedEnvironmentProducts (requestModuleCandidates args))
      Set.empty GeneralCompile Nothing path (requestIncludes args) (requestBuildProductsDir args)
    pure (crWarnings compiled)
  reportDiagsWithWarnings checked

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
                | otherwise -> case sourceFailureDiagnostics exception of
                    Just _ -> runSingletons scope queries
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
          let normalizesWildcards = case query of
                InspectTypeSearch _ -> True
                _ -> False
              purpose = if normalizesWildcards then LookupTypeCompile else GeneralCompile
              key = (normalizesWildcards, path)
          compiled <- case Map.lookup key environments of
            Just previous -> pure previous
            Nothing -> try (compiler CheckedEnvironment Set.empty purpose scope path (requestIncludes args) (requestBuildProductsDir args))
          result <- case compiled of
            Left exception
              | requestInspectionStrict args -> throwIO exception
              | otherwise -> case sourceFailureDiagnostics exception of
                  Just diagnostics ->
                    pure [InspectionRejected (renderInspectionDiagnostics diagnostics)]
                  Nothing -> throwIO exception
            Right successful -> inspect successful [query]
          pure (Map.insert key compiled environments, answers ++ result)

isInspectionTypeQuery :: InspectionRequest -> Bool
isInspectionTypeQuery query = case query of
  InspectTypeOf _ -> True
  _ -> False

-- Both direct checking and GHC dependency loading retain real source
-- diagnostics. Only this structural distinction permits a source alternative;
-- input, protocol and worker failures never choose another template.
sourceFailureDiagnostics :: SomeException -> Maybe [Diag]
sourceFailureDiagnostics exception = case fromException exception of
  Just (sourceError :: SourceError) -> Just (diagsFromSourceError sourceError)
  Nothing -> case fromException exception of
    Just (DependencySourceFailure diagnostics) -> Just diagnostics
    _ -> Nothing

renderInspectionDiagnostics :: [Diag] -> String
renderInspectionDiagnostics = intercalate "\n" . map render
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
        Just (rejection :: InputRejection) -> (ReportInputRejected, [Diag Nothing DiagError (show rejection)])
        Nothing -> case fromException e of
          Just (se :: SourceError) -> (ReportSourceFailure, diagsFromSourceError se)
          Nothing -> case fromException e of
            Just (DependencySourceFailure diagnostics) -> (ReportSourceFailure, diagnostics)
            Just DependencyWorkerFailure -> (ReportWorkerFailure, [diagFromException e])
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
    originalInterfaces <- newOriginalInterfaceArtifacts hscEnv (pprProductInterfaces prepared) outDir
    (preparedArtifacts, productContext) <- prepareArtifacts originalInterfaces caches path hscEnv (pprProductInterfaces prepared) (pprModules prepared) preparedTargets
      (standardAuxiliaryRoots binds) (requestRetainedGenerations args) (pprAcceptedCandidates prepared) (compilationScope <$> pprExactCompilation prepared)
    if null preparedArtifacts
      then ioError (userError "prepared extraction requires --target or --targets")
      else timePhase timing "prepared_sidecars" $ writePreparedSidecars SeparateYieldSites outDir binds tycons mCapturedTy warnTexts preparedArtifacts

    timePhase timing "prepared_write" $ writePreparedArtifacts outDir preparedArtifacts
    timePhase timing "module_products" $
      writeCertifiedProducts originalInterfaces outDir hscEnv prepared productContext preparedArtifacts

  reportDiags res

writeCertifiedProducts
  :: OriginalInterfaceArtifacts -> FilePath -> HscEnv -> PreparedPipelineResult -> Maybe PreparedModuleProducts
  -> [PreparedArtifact] -> IO ()
writeCertifiedProducts originalInterfaces outDir hscEnv prepared productContext preparedArtifacts =
  void (writeCertifiedProductsKeeping originalInterfaces outDir hscEnv prepared productContext preparedArtifacts)

data CertifiedOriginalProducts = CertifiedOriginalProducts
  { certifiedOriginalDependencies :: DependencyEvidence
  , certifiedOriginalProducts :: [ModuleProductEncoding]
  }

writeCertifiedProductsKeeping
  :: OriginalInterfaceArtifacts -> FilePath -> HscEnv -> PreparedPipelineResult -> Maybe PreparedModuleProducts
  -> [PreparedArtifact] -> IO CertifiedOriginalProducts
writeCertifiedProductsKeeping originalInterfaces outDir hscEnv prepared productContext preparedArtifacts = do
    timing <- readTimingEnabled
    (availability, freshProducts) <- timeDetailPhase timing "module_products" "write_products" $
      writeModuleProducts originalInterfaces outDir productContext
        (pprProductInterfaces prepared) (pprPackageImports prepared)
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
    timeDetailPhase timing "module_products" "dependency_evidence" $
      writeDependencyEvidence outDir finalDependencies
    forM_ (pprExactCompilation prepared) $ \compilation -> do
      verified <- revalidateExactScope hscEnv (compilationScope compilation)
      either (ioError . userError) pure verified
      writeExactCompilation compilation freshDependencies
    (productBytes, evidenceBytes) <- timeDetailPhase timing "module_products" "certificate_inputs" $ do
      productBytes <- BS.readFile (outDir </> "module-products.cbor")
      evidenceBytes <- BS.readFile (outDir </> "dependencies.json")
      pure (productBytes, evidenceBytes)
    timeDetailPhase timing "module_products" "certify" $ do
      certified <- encodeCertifiedProducts hscEnv (pprAcceptedCandidates prepared)
        (compilationScope <$> pprExactCompilation prepared)
        (map moduleProductInput freshProducts) [(paTarget artifact, paProgram artifact) | artifact <- preparedArtifacts]
        finalDependencies productBytes evidenceBytes
      case certified of
        Right bytes -> BS.writeFile (outDir </> "certified-products.cbor") bytes
        Left reason -> do
          hPutStrLn stderr ("product certification unavailable: " ++ reason)
          BS.writeFile (outDir </> "certified-products.cbor") BS.empty
    pure (CertifiedOriginalProducts freshDependencies freshProducts)

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
prepareArtifacts :: OriginalInterfaceArtifacts -> RecoveryCaches -> FilePath -> HscEnv -> Map.Map ModuleName ModIface
  -> [PreparedModule] -> [String] -> [String]
  -> Map.Map SymbolIdentity Word64 -> [ModuleCandidate] -> Maybe ExactScope
  -> IO ([PreparedArtifact], Maybe PreparedModuleProducts)
prepareArtifacts _ _ _ _ _ _ [] _ _ _ _ = pure ([], Nothing)
prepareArtifacts originalInterfaces caches input hscEnv interfaces modules targets@(firstTarget : _) auxiliaryRoots retainedGenerations candidates exactScope = do
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
      products = projectOriginalHomeModuleProducts hscEnv interfaces (contextFor firstTarget) modules
      originalProducts =
        [(unitString (moduleUnit owner), moduleNameString (moduleName owner),
          either (Left . show) Right outcome)
        | (owner, outcome) <- preparedModuleProductOutcomes products]
      originalPackageGlobals = requiredOriginalPackageGlobalsWithRetained
        originalProducts candidates exactOriginals (Map.keysSet retainedGenerations)
  recover <- newPreparedRecoveryWithPackageRoots hscEnv (rcFatIface caches) (rcOwnerIface caches)
    (rcPreparedBodies caches) certifiedHomes (contextFor firstTarget) modules []
  artifacts <- forM targets $ \target -> do
    let context = contextFor target
    -- Package roots grow only from the finite exact original-group inventory.
    -- Recovered package code may expose another original group; rescan each
    -- projected target before admitting the final executable closure.
    initial <- timePhase timing "prepared_recover"
      (recover (projectionEntry context))
    let closePackages roots recoveryState = do
          let recovered = preparedRecoveryClosure recoveryState
              finalContext = context
                { projectionAuxiliaryRoots = projectionAuxiliaryRoots context ++ roots }
          selected <- timePhase timing "prepared_project" $
            requireProjection (prepareProjectionWithReachability finalContext
              (closureModules recovered) (closureReachability recovered))
          (program, constructors) <- requireProjection (projectSelected selected)
          required <- either (ioError . userError) pure
            (originalPackageGlobals (programGlobals program))
          let nextRoots = Set.toAscList (Set.fromList (roots ++ required))
          if nextRoots == roots
            then pure (recovered, program, constructors, roots)
            else do
              packageRoots <- forM nextRoots $ \identity -> do
                (identifier, _) <- resolvePackageGlobal hscEnv identity >>= either (ioError . userError) pure
                when (preparedRootIdentity identifier /= identity) $
                  ioError (userError "package recovery root differs from canonical original global")
                pure identifier
              next <- timePhase timing "prepared_recover_original_packages"
                (growPreparedRecovery recoveryState packageRoots)
              closePackages nextRoots next
    (recovered, program, constructors, roots) <- closePackages [] initial
    reportRecoveryResiduals target (closureFailures recovered)
    let defined = Set.fromList
          [identity | group <- programBindings program
          , Execution.TopBinding identity _ <- case group of
              Execution.NonRecursive binding -> [binding]
              Execution.Recursive bindings -> bindings]
    when (any (`Set.notMember` defined) roots) $
      ioError (userError ("required original-group package root has no executable definition: "
        ++ show (filter (`Set.notMember` defined) roots)))
    bytes <- timePhase timing "prepared_encode" $ evaluate (encodeWireProgram program)
    let admitted = Set.fromList (map siteId (programSites program))
        yieldSites =
          [ site
          | preparedModule' <- closureModules recovered
          , site <- pmYieldSites preparedModule'
          , Tidepool.EffectSchema.ysSite site `Set.member` admitted
          ]
    sealedSites <- forM yieldSites $ \site -> do
      witnesses <- forM (Tidepool.EffectSchema.ysInputTypeWitnesses site) $ \witness ->
        maybe (pure Nothing) (sealCheckedTypeWitness originalInterfaces) witness
      pure site { Tidepool.EffectSchema.ysInputTypeWitnesses = witnesses }
    pure (PreparedArtifact target program bytes constructors sealedSites)
  pure (artifacts, Just products)

-- A failed unrelated group is an explicit product miss, never a newly fatal
-- target compile. A complete product pairs every admitted group with the
-- skinny interface emitted by the same GHC transaction.
writeModuleProducts :: OriginalInterfaceArtifacts -> FilePath -> Maybe PreparedModuleProducts
  -> Map.Map ModuleName ModIface
  -> Map.Map ModuleName PackageImportEvidence
  -> IO (Map.Map (String, String) ProductAvailability,
         [ModuleProductEncoding])
writeModuleProducts _ _ Nothing _ _ = pure (Map.empty, [])
writeModuleProducts originalInterfaces outDir (Just inventory) interfaces packageRoots = do
  timing <- readTimingEnabled
  outcomes <- forM (preparedModuleProductOutcomes inventory) $ \(owner, outcome) -> do
    let name = moduleName owner
        key = (unitString (moduleUnit owner), moduleNameString name)
    case Map.lookup name interfaces >>= \interface ->
        if mi_module interface == owner then Just interface else Nothing of
      Nothing -> do
        hPutStrLn stderr ("module product unavailable: no interface for " ++ moduleNameString name)
        pure (key, ProductMissingInterface, Nothing, Nothing)
      Just _ -> case outcome of
        Left reason -> do
          hPutStrLn stderr ("module product unavailable: " ++ moduleNameString name
            ++ ": " ++ show reason)
          pure (key, ProductProjectionRejected, Nothing, Nothing)
        Right groups -> do
          bytes <- timeDetailPhase timing "module_products.interfaces" (snd key) $
            originalInterfaceBytes originalInterfaces owner
              >>= maybe (fail "original product interface lacks its captured artifact") pure
          roots <- case Map.lookup name packageRoots of
            Nothing -> ioError (userError
              ("resolved direct package import inventory missing for " ++ moduleNameString name))
            Just selected -> pure selected
          let iface = ExactIfaceArtifact (fst key) (snd key) ""
                (shaHex bytes) []
              sidecar = encodePackageImports iface roots
          when (BS.length sidecar > 4 * 1024 * 1024) $
            ioError (userError "direct package import witness exceeds four MiB")
          pure (key, ProductReady, Just (prepareModuleProductEncoding (T.pack (fst key),
            T.pack (snd key), bytes, groups)), Just sidecar)
  let products = [moduleProduct | (_, _, Just moduleProduct, _) <- outcomes]
      packageBundles =
        [(unit, moduleName', sidecar)
        | ((unit, moduleName'), _, Just _, Just sidecar) <- outcomes]
  timeDetailPhase timing "module_products" "encode_products" $
    BS.writeFile (outDir </> "module-products.cbor") (encodeModuleProductInventory products)
  timeDetailPhase timing "module_products" "encode_package_bundles" $
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
runTurnMode :: Compiler -> RecoveryCaches -> WorkerRequest -> FilePath -> IO ExitCode
runTurnMode compiler caches args path = do
  timing <- readTimingEnabled
  hPutStrLn stderr $ "Processing (turn): " ++ path
  lastAttempt <- newIORef Nothing
  res <- timePhase timing "total" $ trySynchronous $ do
    turnSrc   <- readFile path
    let templates = requestTurnTemplates args
    mVerdict  <- traverse parseTurnVerdictArg (requestTurnVerdict args)
    parserFlags <- case mVerdict of
      Nothing -> Just <$> defaultParserDynFlags
      Just (StmtBinders { sbKind = KDecl }) -> Just <$> defaultParserDynFlags
      Just _ -> pure Nothing
    let requireParserFlags = maybe
          (error "turn parser flags were not initialized") id parserFlags
    -- The parse emits no phases of its own. This mode times it as the single
    -- @classify@ phase, emitted
    -- only on the branch that actually classifies. With @--turn-verdict@
    -- supplied nothing is parsed, and an absent @classify@ row is the
    -- honest report rather than a phantom 0ms line.
    sb        <- maybe (timePhase timing "classify"
      (evaluate (classifyWithFlags requireParserFlags turnSrc))) return mVerdict
    exact <- traverse (\manifest -> readExactScope manifest >>= either fail pure) (requestSessionArtifacts args)
    let admittedItem = exact >>= scopeCheckedItem
    forM_ admittedItem $ \admission -> validateCheckedItemAdmission args admission turnSrc sb
    let admittedDisplay = exact >>= scopeCheckedDisplay
    forM_ admittedDisplay $ \admission -> validateCheckedDisplayAdmission args admission turnSrc sb
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
        declarationSource <- declarationSourceWithTemplateFlags requireParserFlags tmplSrc turnSrc
          >>= either throwCellSplitError pure
        spliced <- either fail pure (renderDeclarationForTemplate tmplSrc declarationSource)
        (_spliced, modName, modulePath) <- writeSplicedModule outDir lastAttempt spliced
        items <- timePhase timing "declaration_binders" $ extractBindersNamed modulePath (requestIncludes args) modName
        let binders = if null (sbBinders sb)
                        then map (T.pack . exportItemName) items
                        else map T.pack (sbBinders sb)
        return (TDecl binders items declarationSource)
      _kind -> compileClassifiedTurn compiler caches args timing outDir turnSrc sb bindersStr [] admittedItem admittedDisplay lastAttempt
    outFile <- requireArg "--turn-out" (requestTurnOut args)
    let cbor = encodeTurnOut turnOut
    BS.writeFile outFile cbor
    forM_ (exact >>= \scope -> (,) scope <$> scopeCheckedItem scope) $ \(scope,admission) ->
      case turnOut of
        TBind _ _ _ _ wrapped -> writeCheckedItemReceipt outDir scope admission (T.unpack wrapped)
        TExpr _ _ wrapped -> writeCheckedItemReceipt outDir scope admission (T.unpack wrapped)
        TDecl {} -> fail "checked recipe cannot compile an unproved declaration"
    forM_ (exact >>= \scope -> (,) scope <$> scopeCheckedDisplay scope) $ \(scope,admission) ->
      case turnOut of
        TBind _ _ _ _ wrapped -> writeCheckedDisplayReceipt outDir scope admission (T.unpack wrapped)
        _ -> fail "checked display did not produce its capture bundle"
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
  -> String -> StmtBinders -> String -> [String] -> Maybe CheckedItemAdmission -> Maybe CheckedDisplayAdmission
  -> IORef (Maybe (FilePath, String))
  -> IO TurnOut
compileClassifiedTurn compiler caches args timing outDir turnSrc sb bindersStr typeImports admitted display lastAttempt =
  compiledTurn <$> compileClassifiedTurnKeeping compiler caches args timing outDir turnSrc sb bindersStr typeImports admitted display lastAttempt [] Nothing

data CompiledTurnOutput = CompiledTurnOutput
  { compiledTurn :: TurnOut
  , compiledPipeline :: PreparedPipelineResult
  , compiledOriginalProducts :: CertifiedOriginalProducts
  , compiledModule :: String
  }

compileClassifiedTurnKeeping
  :: Compiler -> RecoveryCaches -> WorkerRequest -> Bool -> FilePath
  -> String -> StmtBinders -> String -> [String] -> Maybe CheckedItemAdmission -> Maybe CheckedDisplayAdmission
  -> IORef (Maybe (FilePath, String)) -> [String] -> Maybe SourcePrologue -> IO CompiledTurnOutput
compileClassifiedTurnKeeping compiler caches args timing outDir turnSrc sb bindersStr typeImports admitted display lastAttempt programImports prologue = do
    let templates = requestTurnTemplates args
        protectedTemplates = case (display,admitted) of
          (Just authority,Nothing) -> Just (displayTurnTemplates authority)
          (Nothing,Just authority) -> Just (itemTurnTemplates authority)
          _ -> Nothing
        verifyProtectedTemplate path captured = forM_ protectedTemplates $ \expected ->
          unless (any (\(kind,file) -> file == path
              && lookup kind expected == Just (shaHex (TE.encodeUtf8 (T.pack captured)))) templates)
            (fail "generated scaffold template differs from its protected offer")
        basePurpose = case (display, admitted) of
          (Just authority, Nothing) -> CheckedItemCompile [] (displayPlannedDeclaration authority) (displayCompletedValues authority)
          (Nothing, Just authority) -> checkedItemCompilePurpose authority
          _ -> GeneralCompile
        -- Splice @tmplFile@ against the turn text, write the spliced module
        -- to a scratch file under 'outDir', and return it alongside the
        -- module name derived from its own @module X where@ header. The
        -- scratch file's basename must match that header — 'runPipelineSessionSelected'
        -- looks up the compiled module by @capitalize (takeBaseName path)@
        -- (GhcPipeline.hs) exactly as 'tidepool_runtime::extract_module_name'
        -- does today for the existing two-spawn wrap_* templates
        -- (session.rs), which this mode's templates carry over unchanged.
        spliceInto :: FilePath -> IO (String, String, String, FilePath)
        spliceInto tmplFile = do
          originalTemplate <- readFile tmplFile
          verifyProtectedTemplate tmplFile originalTemplate
          tmplSrc <- case prologue of
            Nothing -> pure originalTemplate
            Just authored -> do
              withImports <- replaceRecipeMarker "default (Int, Double, Text)\n"
                (concatMap ((++ "\n") . locatedImportSource) (prologueImports authored)
                  ++ "default (Int, Double, Text)\n") originalTemplate
              pure (concatMap ((++ "\n") . locatedPragmaSource) (prologuePragmas authored) ++ withImports)
          let original = case (display, admitted) of
                (Just authority, Nothing) -> displayPlannedDeclaration authority
                (Nothing, Just authority) -> itemPlannedDeclaration authority
                _ -> Nothing
          withOriginal <- case original of
            Nothing -> pure tmplSrc
            Just ((_, owner), _) -> replaceRecipeMarker "default (Int, Double, Text)\n"
              ("import " ++ owner ++ "\ndefault (Int, Double, Text)\n") tmplSrc
          withProgram <- if null programImports then pure withOriginal else
            replaceRecipeMarker "default (Int, Double, Text)\n"
              (concatMap (\owner -> "import " ++ owner ++ "\n") programImports ++ "default (Int, Double, Text)\n") withOriginal
          let renderRecipe preview = do
                -- Replace only the protected scaffold marker, before inserting
                -- the admitted statement or checked signature declarations.
                withPreview <- maybe (pure withProgram)
                  (\body -> replaceRecipeMarker "{{ACTIVATION_PREVIEW}}" body withProgram) preview
                tmplWithImports <- insertCheckedTypeImports typeImports withPreview
                case (display, admitted) of
                  (Just admission, Nothing) -> if requestCell args
                    then checkedProgramDisplayRecipe admission tmplWithImports
                    else checkedDisplayRecipe admission tmplWithImports
                  (Nothing, Nothing) -> pure (spliceTemplate tmplWithImports turnSrc bindersStr)
                  (Just _, Just _) -> fail "display and item authority cannot share one recipe"
                  (Nothing, Just admission) -> do
                    withPrefix <- if null (itemValueImports admission) then pure tmplWithImports else
                      replaceRecipeMarker "default (Int, Double, Text)\n"
                        (concatMap (\(moduleName',names) -> "import " ++ moduleName' ++ " (" ++ intercalate ", " (map renderProgramBinder names) ++ ")\n")
                          (itemValueImports admission) ++ "default (Int, Double, Text)\n") tmplWithImports
                    checkedRecipeSource admission withPrefix turnSrc
          rendered <- if requestActivationPreview args
            then do
              let opaque = "(TidepoolScaffoldText.pack \"<opaque value>\\nUse the input type to select fields or apply sessionInput.\", False)"
              checkSource <- renderRecipe (Just opaque)
              (_, checkModule, checkPath) <- writeSplicedModule outDir lastAttempt checkSource
              checkPurpose <- case protectedTemplates of
                Nothing -> pure basePurpose
                Just _ -> do
                  recipe <- generatedScaffoldRecipe originalTemplate checkSource checkPath checkModule >>= either fail pure
                  pure (GeneratedScaffoldCompile recipe basePurpose)
              checked <- compiler CheckedEnvironment Set.empty checkPurpose
                (Just (scopeFromWorkerRequest args)) checkPath (requestIncludes args) (requestBuildProductsDir args)
              inputType <- maybe (fail "activation is missing its checked input type") (pure . stripMonadHead) (crResultType checked)
              rendered <- satisfiesCapturedConstraint (crHscEnv checked) (crTargetTcGblEnv checked)
                "__tidepoolActivationConstraint" inputType
              finalSource <- renderRecipe (Just (if rendered
                then "TidepoolInspection.workbenchActivationDisplay __activationBudget __activationInput"
                else opaque))
              writeSplicedModule outDir lastAttempt finalSource
            else renderRecipe Nothing >>= writeSplicedModule outDir lastAttempt
          let (source,moduleName',modulePath) = rendered
          pure (originalTemplate,source,moduleName',modulePath)
    -- Four-shape selection (protocol note, "the verdict space has four
    -- shapes, not three"): a bind that binds no name selects its own
    -- template kind and skips the session-bind artifacts entirely —
    -- 'templateSelectorForVerdict' mirrors Rust's
    -- 'TemplateSelector::for_verdict' exactly.
    let selector = templateSelectorForVerdict (sbKind sb) (sbBinders sb)
    let scope = scopeFromWorkerRequest args
        allMatching = [f | (name, f) <- templates, name == templateSelectorWireName selector]
        matching = case admitted of
          Just admission | itemKind admission == "expr" ->
            [(0,file) | (kind,file) <- templates, kind == "bind",
              length [() | (candidate,_) <- templates, candidate == "bind"] == 1,
              isJust (itemObservationName admission)]
          _ -> zip [0..] allMatching
        -- A prepared turn uses one compiler pass for the checked metadata
        -- and the prepared modules.
        compileTurn protected spliced modName modulePath = do
          purpose <- case protectedTemplates of
            Nothing -> pure basePurpose
            Just _ -> do
              recipe <- generatedScaffoldRecipe protected spliced modulePath modName >>= either fail pure
              pure (GeneratedScaffoldCompile recipe basePurpose)
          compiler (PreparedProducts (requestModuleCandidates args))
            (Map.keysSet (requestRetainedGenerations args)) purpose
            (Just scope) modulePath (requestIncludes args)
            (requestBuildProductsDir args)
        compileVariants _ [] = error ("--turn: no --turn-template for kind " ++ templateSelectorWireName selector)
        compileVariants _ ((index,tmplFile):rest) = do
          (protected, spliced, modName, modulePath) <- spliceInto tmplFile
          attempted <- try (compileTurn protected spliced modName modulePath)
          case attempted of
            Right prepared ->
              return (index, spliced, modulePath, prepared)
            Left err@(_ :: SomeException) -> case (sourceFailureDiagnostics err, rest) of
              (Just _, _ : _) | not (isJust admitted || isJust display) -> compileVariants (index + 1) rest
              _               -> throwIO err
    if requestActivationPreview args
        && (selector /= SBind || length (sbBinders sb) /= 1 || length matching /= 1)
      then fail "activation requires one prepared bind template"
      else pure ()
    (variant, spliced, compiledPath, prepared) <- compileVariants (0 :: Int) matching
    let rawResult = pprPipelineResult prepared
        result = if requestCell args && isJust display then rawResult
          { prResultType = prResultType rawResult >>= \ty -> case splitFunTy_maybe ty of
              Just (_,_,_,body) -> Just body
              Nothing -> Nothing }
          else rawResult
        preparedModules = pprModules prepared
        binds       = prBinds result
        hscEnv      = prHscEnv result
        mCapturedTy = fmap T.pack (prCapturedType result)
        warnTexts   = map T.pack (prWarnings result)
    -- Projection remains outside compileVariants. Its entry is the settled
    -- scaffold, and its constructors join the shared metadata before write.
    originalInterfaces <- newOriginalInterfaceArtifacts hscEnv (pprProductInterfaces prepared) outDir
    (preparedArtifacts, productContext) <- prepareArtifacts originalInterfaces caches compiledPath hscEnv (pprProductInterfaces prepared) preparedModules
      [preparedScaffoldTargetName] (standardAuxiliaryRoots binds)
      (requestRetainedGenerations args) (pprAcceptedCandidates prepared) (compilationScope <$> pprExactCompilation prepared)
    when (maybe False ((== HostActivationInput) . itemPurpose) admitted) $ do
      input <- either fail pure (activationPreviewInputType (prTargetTcGblEnv result))
      witness <- maybe (fail "activation input type has no complete canonical witness") pure
        (captureCheckedTypeWitness hscEnv input)
      sealed <- sealCheckedTypeWitness originalInterfaces witness
        >>= maybe (fail "activation input type lacks an original owner interface seal") pure
      encoded <- maybe (fail "activation input type witness is unsealed") pure (encodeCheckedTypeWitness sealed)
      BS.writeFile (outDir </> "activation-type.cbor") (toStrictByteString encoded)
    let asksSites = concatMap paYieldSites preparedArtifacts
    timePhase timing "prepared_sidecars" $ writePreparedSidecars InlineYieldSites outDir binds (prTyCons result) mCapturedTy warnTexts preparedArtifacts
    timePhase timing "prepared_write" $ writePreparedArtifacts outDir preparedArtifacts
    originalProducts <- timePhase timing "module_products" $
      writeCertifiedProductsKeeping originalInterfaces outDir hscEnv prepared productContext preparedArtifacts
    when (not (requestCell args) && not (requestActivationPreview args)
        && null (requestInjectVals args) && not (isJust (requestSessionArtifacts args))
        && Map.null (requestRetainedGenerations args)) $
      writeCompileInputProof outDir hscEnv (pprDependencies prepared) (pprPackageImports prepared)
    -- Mutable turns never enter the artifact cache, but publication must
    -- still reject source changes observed during this compilation.
    validateDependencyEvidence (pprDependencies prepared)
    let wrapped = T.pack spliced
    turn <- case selector of
      SBind -> do
        g    <- requireArg "--bind-gen"     (requestBindGen args)
        root <- requireArg "--session-root" (requestSessionRoot args)
        bbs  <- mkBoundBinders (sbBinders sb) g root result
        return (TBind (map T.pack (sbBinders sb)) variant bbs asksSites wrapped)
      SBindDiscard -> return (TBind [] variant [] asksSites wrapped)
      SExpr -> case admitted >>= itemObservationName of
        Just observation -> do
          generation <- requireArg "--bind-gen" (requestBindGen args)
          root <- requireArg "--session-root" (requestSessionRoot args)
          bound <- mkBoundBinders [observation] generation root result
          return (TBind [T.pack observation] variant bound asksSites wrapped)
        Nothing -> return (TExpr variant asksSites wrapped)
      SDecl -> error ("--turn: unexpected verdict kind: " ++ templateSelectorWireName selector)
    owner <- maybe (fail "compiled turn has no module owner") pure (extractModuleName spliced)
    pure (CompiledTurnOutput turn prepared originalProducts owner)

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
-- Parsing chooses execution ordinals before runtime reserves original owners.
-- The emitted receipt contains no checked pins, prepared bodies or live values.
runCellPlanMode :: WorkerRequest -> FilePath -> IO ExitCode
runCellPlanMode args cellPath = do
  result <- trySynchronous $ do
    source <- readFile cellPath
    templatePath <- requireArg "--cell-template" (requestCellTemplate args)
    template <- readFile templatePath
    templates <- forM (requestTurnTemplates args) $ \(kind,path) -> do
      bytes <- BS.readFile path
      pure (kind, shaHex bytes)
    plan <- analyzeOrderedCell template source >>= either throwCellSplitError pure
    let text = encodeString . T.pack
        observation = encodeCellOut plan [] [] ""
        receipt = encodeListLen 7 <> text "TPCELLPLAN1"
          <> text (shaHex (TE.encodeUtf8 (T.pack source)))
          <> text (shaHex (TE.encodeUtf8 (T.pack template)))
          <> encodeListLen (fromIntegral (length templates))
          <> foldMap (\(kind,digest) -> encodeListLen 2 <> text kind <> text digest) templates
          <> encodeListLen (fromIntegral (length (requestIncludes args)))
          <> foldMap text (requestIncludes args)
          <> encodeListLen (fromIntegral (length (requestInjectVals args)))
          <> foldMap text (requestInjectVals args)
          <> encodeBytes observation
    out <- requireArg "--cell-out" (requestCellOut args)
    BS.writeFile out (toStrictByteString receipt)
  reportDiags result

runCellMode :: Compiler -> RecoveryCaches -> WorkerRequest -> FilePath -> IO ExitCode
runCellMode compiler caches args cellPath = do
  exact <- traverse (\manifest -> readExactScope manifest >>= either fail pure) (requestSessionArtifacts args)
  case exact >>= \scope -> (,) scope <$> (scopeCheckedCell scope >>= checkedPlannedCell) of
    Just (scope, planned) -> runCellProgramMode compiler caches args cellPath scope planned
    Nothing -> runLegacyCellMode compiler caches args cellPath

runLegacyCellMode :: Compiler -> RecoveryCaches -> WorkerRequest -> FilePath -> IO ExitCode
runLegacyCellMode compiler caches args cellPath = do
  timing <- readTimingEnabled
  provisionalOutput <- newIORef Nothing
  res <- trySynchronous $ do
    cellSource <- readFile cellPath
    templatePath <- requireArg "--cell-template" (requestCellTemplate args)
    template <- readFile templatePath
    admittedScope <- traverse (\manifest -> readExactScope manifest >>= either fail pure)
      (requestSessionArtifacts args)
    forM_ admittedScope $ \scope -> forM_ (scopeCheckedCell scope) $ \admission ->
      validateCheckedCellAdmission args admission cellSource template
    checkingTemplate <- case admittedScope >>= scopeCheckedCell of
      Nothing -> pure template
      Just _ -> either (throwIO . InvalidCheckingWrapper) pure
        (replaceTemplateModuleHeader "module CellCheck where" template)
    initialPlan <- analyzeCell template cellSource >>= either throwCellSplitError pure
    initialSource <- either fail pure (renderCellCheckSource checkingTemplate initialPlan)
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
    preparedDeclaration <- case admittedScope >>= \exact -> (,) exact <$> scopeCheckedCell exact of
      Just (exact, admission)
        | any ((== KDecl) . sbKind . cellAnalysisVerdict) (cellPlanItems initialPlan) ->
            Just <$> prepareOriginalCellDeclaration compiler caches args template outDir scope exact admission initialPlan
      _ -> pure Nothing
    let checkPurpose = maybe GeneralCompile (\(_,_,inventory,exact) -> PlannedDeclarationCheck inventory exact) preparedDeclaration
        checkedSelection = if isJust preparedDeclaration then CheckedEnvironment else
          maybe CheckedEnvironment CheckedEnvironmentProducts (requestModuleCandidates args)
        checkPlan plan = maybe plan (\(_,planned,_,_) -> plannedCheckPlan planned) preparedDeclaration
    (analyzed, provisional) <- checkCellInstances (\plan -> do
      let effective = checkPlan plan
      rendered <- either fail pure (renderCellCheckSource checkingTemplate effective)
      writeFile modulePath rendered
      -- Preserve the latest plan for failure diagnostics without encoding and
      -- writing a provisional result before every successful check attempt.
      writeIORef provisionalOutput (Just (out, plan, rendered))
      compiler checkedSelection Set.empty checkPurpose scope modulePath (requestIncludes args) (requestBuildProductsDir args))
        (maybe initialPlan (\(plan,_,_,_) -> plan) preparedDeclaration)
    checkedSource <- either fail pure (renderCellCheckSource checkingTemplate (checkPlan analyzed))
    (finalPlan, finalSource, compiled) <- if isJust preparedDeclaration || null (cellPlanDisplayTargets analyzed)
      then pure (analyzed, checkedSource, provisional)
      else do
        contextDeclarations <- cellDisplayDeclarations DisplayInstanceContexts provisional analyzed
        let contextual = installCellDisplayDeclarations contextDeclarations analyzed
        contextualSource <- either fail pure (renderCellCheckSource checkingTemplate contextual)
        writeFile modulePath contextualSource
        contextChecked <- compiler (maybe CheckedEnvironment CheckedEnvironmentProducts (requestModuleCandidates args)) Set.empty GeneralCompile scope modulePath (requestIncludes args) (requestBuildProductsDir args)
        declarations <- cellDisplayDeclarations DisplayInstanceFields contextChecked analyzed
        let finalized = installCellDisplayDeclarations declarations analyzed
        finalizedSource <- either fail pure (renderCellCheckSource checkingTemplate finalized)
        writeFile modulePath finalizedSource
        finalizedResult <- compiler (maybe CheckedEnvironment CheckedEnvironmentProducts (requestModuleCandidates args)) Set.empty GeneralCompile scope modulePath (requestIncludes args) (requestBuildProductsDir args)
        pure (finalized, finalizedSource, finalizedResult)
    -- Statement preparation checks these rendered pins in their actual value
    -- modules before any declaration commits or effect runs.
    expressionEvidence <- cellExpressionEvidence compiled
    let outputBytes = encodeCellOut finalPlan (crCheckedBinderPins compiled)
          (map fst expressionEvidence) finalSource
    BS.writeFile out outputBytes
    forM_ admittedScope $ \receiptScope -> forM_ (scopeCheckedCell receiptScope) $ \admission -> do
      binderSignatures <- cellCheckedBinderSignatures compiled
      cellNow <- readFile cellPath
      templateNow <- readFile templatePath
      validateCheckedCellAdmission args admission cellNow templateNow
      verified <- revalidateExactScope (crHscEnv compiled) receiptScope
      either fail pure verified
      plannedReceipt <- traverse (const (BS.readFile (outDir </> "planned-declaration.cbor"))) preparedDeclaration
      let text = encodeString . T.pack
          signatures = binderSignatures ++ map snd expressionEvidence
          receipt = encodeListLen 10
            <> text "TPEXACTCHECK" <> text "1" <> text (scopeRequestSha256 receiptScope)
            <> text (checkedAdmissionDigest admission) <> text (checkedCellSha256 admission)
            <> text (checkedTemplateSha256 admission) <> text (shaHex outputBytes)
            <> text (shaHex (TE.encodeUtf8 (T.pack finalSource)))
            <> encodeListLen (fromIntegral (length signatures))
            <> foldMap encodeCheckedSignature signatures
            <> maybe encodeNull (text . shaHex) plannedReceipt
      BS.writeFile (outDir </> "checked-cell.cbor") (toStrictByteString receipt)
    -- Fold preparation is optional and cannot change a successful check.
    -- Its typed outcome distinguishes ineligibility from a real attempt;
    -- any partial artifacts of a failed attempt remain diagnostic only.
    foldOutcome <- if requestCellFoldTurn args
      then attemptCellFoldTurn compiler caches args timing outDir finalPlan compiled admittedScope
      else pure CellFoldNotRequested
    BS.writeFile (outDir </> "cell-fold.cbor") (encodeCellFoldOutcome foldOutcome)
    pure (crWarnings compiled)
  case res of
    Left _ -> do
      provisional <- readIORef provisionalOutput
      forM_ provisional $ \(out, plan, rendered) -> do
        _ <- try (BS.writeFile out (encodeCellOut plan [] [] rendered)) :: IO (Either IOException ())
        pure ()
    Right _ -> pure ()
  reportDiagsWithWarnings res

-- The worker owns all GHC passes of a cell. Interfaces produced here are
-- type evidence; no value or effect is evaluated by this transaction.
data ProgramCellState = ProgramCellState
  { programPrologue :: SourcePrologue
  , programExact :: ExactScope
  , programValues :: [CompletedValueImport]
  , programOriginal :: Maybe ((String,String),String)
  , programOriginals :: [((String,String),String)]
  , programRetained :: Map.Map SymbolIdentity Word64
  , programPlans :: [CellSourcePlan]
  , programPins :: [CheckedBinderPin]
  , programExpressions :: [CellExpressionPlan]
  , programCheckedSignatures :: [CheckedSignature]
  , programSources :: [String]
  , programDeclarations :: [(Int,String)]
  }

runCellProgramMode :: Compiler -> RecoveryCaches -> WorkerRequest -> FilePath
  -> ExactScope -> PlannedCellAdmission -> IO ExitCode
runCellProgramMode compiler caches args cellPath exact planned = do
  timing <- readTimingEnabled
  attempted <- trySynchronous $ do
    source <- readFile cellPath
    templatePath <- requireArg "--cell-template" (requestCellTemplate args)
    template <- readFile templatePath
    admission <- maybe (fail "compiled cell has no source admission") pure (scopeCheckedCell exact)
    validateCheckedCellAdmission args admission source template
    parserBytes <- BS.readFile (plannedParserPath planned)
    unless (shaHex parserBytes == plannedParserSha256 planned
        && plannedReservationDigest planned == checkedAdmissionDigest admission)
      (fail "compiled cell parser or reservation changed")
    initial <- analyzeOrderedCell template source >>= either throwCellSplitError pure
    unless (length (cellPlanItems initial) == length (plannedSlots planned))
      (fail "compiled cell reservation count differs from parser")
    root <- requireArg "--session-root" (requestSessionRoot args)
    let outDir = fromMaybe (takeDirectory cellPath </> "cell-program") (requestOutDir args)
        initialState = ProgramCellState (cellPlanPrologue initial) exact [] Nothing [] (requestRetainedGenerations args)
          [] [] [] [] [] []
    createDirectoryIfMissing True outDir
    settled <- foldM (compileSegment timing admission template root outDir)
      initialState (zip [0::Int ..] (cellInferenceSegments initial))
    let finalPlan = initial { cellPlanItems = concatMap cellPlanItems (programPlans settled) }
        checkedSource = concat (programSources settled)
        observations = encodeCellOut finalPlan (programPins settled) (programExpressions settled) checkedSource
        text = encodeString . T.pack
        receipt = encodeListLen 11 <> text "TPEXACTPROGRAM" <> text "1"
          <> text (scopeRequestSha256 exact) <> text (checkedAdmissionDigest admission)
          <> text (checkedCellSha256 admission) <> text (checkedTemplateSha256 admission)
          <> text (shaHex observations) <> text (shaHex (TE.encodeUtf8 (T.pack checkedSource)))
          <> encodeListLen (fromIntegral (length (programCheckedSignatures settled)))
          <> foldMap encodeCheckedSignature (programCheckedSignatures settled)
          <> encodeListLen (fromIntegral (length (programDeclarations settled)))
          <> foldMap (\(index,digest) -> encodeListLen 2 <> encodeWord64 (fromIntegral index) <> text digest)
            (programDeclarations settled)
          <> text (plannedParserDigest planned)
    out <- requireArg "--cell-out" (requestCellOut args)
    BS.writeFile out observations
    BS.writeFile (outDir </> "checked-cell.cbor") (toStrictByteString receipt)
    BS.writeFile (outDir </> "cell-fold.cbor") (encodeCellFoldOutcome CellFoldNotRequested)
    sourceNow <- readFile cellPath
    templateNow <- readFile templatePath
    validateCheckedCellAdmission args admission sourceNow templateNow
    unless (sourceNow == source && templateNow == template) (fail "compiled cell source changed")
    pure []
  reportDiagsWithWarnings attempted
  where
    compileSegment timing admission template root outDir state (segmentIndex, segment) = do
      let offset = sum (map (length . cellPlanItems) (programPlans state))
          prefix = selectedProgramValues (programValues state)
          withPrefix = installProgramImports prefix (programOriginals state) segment
          scope = programExact state
          localArgs = args { requestInjectVals = map exactModule (maybe [] checkedValueInterfaces (scopeCheckedCell scope))
            , requestRetainedGenerations = programRetained state }
          scoped :: Compiler
          scoped selection retained purpose session path includes products =
            compiler selection retained (CellProgramCompile (programPurpose state purpose) scope) session path includes products
          directory = outDir </> "segment-" ++ show segmentIndex
      createDirectoryIfMissing True directory
      case cellPlanItems segment of
        [item] | sbKind (cellAnalysisVerdict item) == KDecl -> do
          generation <- case plannedSlots planned !! offset of
            PlannedDeclaration value -> pure value
            PlannedPrologue value -> pure value
            _ -> fail "declaration has a native reservation"
          let owner = "Tidepool.Session.Lib.G" ++ show generation
              ownAdmission = admission { checkedReservedModules = [owner] }
          (finalized,_,inventory,extended) <- timePhase timing "cell_program_declaration" $
            prepareOriginalCellDeclaration scoped caches localArgs template directory
              (Just (scopeFromWorkerRequest localArgs)) scope ownAdmission withPrefix
          receipt <- BS.readFile (directory </> "planned-declaration.cbor")
          let original = Just (("main",owner), extractPlannedFingerprint inventory)
          -- Keep the ordered item under its original ordinal; its original
          -- module owns declarations and the generated instances.
          let declared = [exportOccurrence identity | exported <- plannedExports inventory
                , identity <- exportHead exported : exportChildren exported
                , exportNamespace identity /= TypeNamespace]
              remainingValues = [value { completedValueBinders = kept }
                | value <- programValues state
                , let kept = [(name,identifier) | (name,identifier) <- completedValueBinders value, name `notElem` declared]
                , not (null kept)]
          pure state { programExact = extended, programOriginal = original
            , programOriginals = programOriginals state ++ maybe [] pure original
            , programValues = remainingValues
            , programPlans = programPlans state ++ [finalized]
            , programSources = programSources state ++ [plannedSourceFromDirectory finalized]
            , programDeclarations = programDeclarations state ++ [(offset,shaHex receipt)] }
        _ -> do
          let checkPath = directory </> "CellCheck.hs"
              checkingTemplate = either error id (replaceTemplateModuleHeader "module CellCheck where" template)
              check plan = do
                rendered <- either fail pure (renderCellCheckSource checkingTemplate plan)
                let globalSource = globalProgramKeys offset (length (cellPlanItems plan)) rendered
                writeFile checkPath globalSource
                scoped (maybe CheckedEnvironment CheckedEnvironmentProducts (requestModuleCandidates localArgs)) Set.empty
                  (CheckedItemCompile [] (programOriginal state) prefix)
                  (Just (scopeFromWorkerRequest localArgs)) checkPath (requestIncludes args) (requestBuildProductsDir args)
          (checked,compiled) <- timePhase timing "cell_program_segment_check" (checkCellInstances check withPrefix)
          signatures <- cellCheckedBinderSignatures compiled
          expressions <- cellExpressionEvidence compiled
          rendered <- either fail pure (renderCellCheckSource checkingTemplate checked)
          let globalSource = globalProgramKeys offset (length (cellPlanItems checked)) rendered
              checkedState = state { programPlans = programPlans state ++ [checked]
                , programPins = programPins state ++ crCheckedBinderPins compiled
                , programExpressions = programExpressions state ++ map fst expressions
                , programCheckedSignatures = programCheckedSignatures state ++ signatures ++ map snd expressions
                , programSources = programSources state ++ [globalSource] }
          foldM (compileNative timing admission root outDir)
            checkedState (zip [offset..] (cellPlanItems checked))

    compileNative timing admission root outDir state (index,item) = do
      let prefix = selectedProgramValues (programValues state)
          scope = programExact state
          slot = plannedSlots planned !! index
          (generation,observation) = case slot of
            PlannedBind value -> (value,Nothing)
            PlannedExpression capture _ name -> (capture,Just name)
            _ -> error "native item has declaration reservation"
          source = cellAnalysisSource item
          verdict = cellAnalysisVerdict item
          keys = case sbKind verdict of
            KBind -> ["__tidepool_cell_pin_" ++ show index ++ "_" ++ binder | binder <- sbBinders verdict]
            KExpr -> ["__tidepool_cell_expr_" ++ show index]
            KDecl -> []
          signatures = [signature | key <- keys, signature <- programCheckedSignatures state, signatureKey signature == key]
          expression = case [value | value <- programExpressions state, expressionPlanKey value `elem` keys] of
            [value] -> Just value
            _ -> Nothing
          itemAdmission = CheckedItemAdmission AuthoredCheckedItem (checkedAdmissionDigest admission) (checkedAdmissionDigest admission)
            (fromIntegral index) (shaHex (TE.encodeUtf8 (T.pack source)))
            (if sbKind verdict == KBind then "bind" else "expr") (sbBinders verdict)
            (checkedTurnTemplates admission) (map exactModule (scopeValues scope)) signatures
            (fmap (\value -> case expressionPlanLift value of ExpressionPure -> "pure"; ExpressionEffectful -> "effectful") expression)
            (fmap (\value -> case expressionPlanPresentation value of ExpressionRendered -> "rendered"; ExpressionOpaque -> "opaque") expression)
            generation (checkedAdmissionDigest admission) (map programValueImport prefix) observation (programOriginal state)
            prefix (scopeValues scope)
          localArgs = args { requestBindGen = Just generation, requestSessionRoot = Just root
            , requestInjectVals = map exactModule (scopeValues scope)
            , requestRetainedGenerations = programRetained state }
          scoped :: Compiler
          scoped selection retained purpose session path includes products =
            compiler selection retained (CellProgramCompile (programPurpose state purpose) scope) session path includes products
          directory = outDir </> "item-" ++ show index
      createDirectoryIfMissing True directory
      validateCheckedItemAdmission localArgs itemAdmission source verdict
      lastAttempt <- newIORef Nothing
      output <- timePhase timing "cell_program_native" $ compileClassifiedTurnKeeping scoped caches localArgs timing directory
        source verdict (intercalate ", " (sbBinders verdict)) [] (Just itemAdmission) Nothing lastAttempt (priorProgramImports state) (Just (programPrologue state))
      extended <- retainProgramProducts (requestIncludes args) directory (compiledPipeline output)
        (compiledOriginalProducts output) (compiledModule output) scope
      let turn = compiledTurn output
          retainedState = state { programExact = extended }
      BS.writeFile (directory </> "turn.cbor") (encodeTurnOut turn)
      case turn of
        TBind _ _ binders _ wrapped -> do
          writeCheckedItemReceipt directory scope itemAdmission (T.unpack wrapped)
          next <- addProgramValue root generation binders retainedState
          case (slot,expression) of
            (PlannedExpression _ display name,Just expressionPlan) ->
              compileDisplay timing admission root outDir index display name expressionPlan next
            _ -> pure next
        _ -> fail "compiled cell native recipe did not return bind metadata"

    compileDisplay timing admission root outDir index generation observation expression state = do
      let scope = programExact state
          prefix = selectedProgramValues (programValues state)
          capture = case plannedSlots planned !! index of
            PlannedExpression value _ _ -> value
            _ -> error "display has no capture slot"
          display = CheckedDisplayAdmission (checkedAdmissionDigest admission) (checkedAdmissionDigest admission)
            (fromIntegral index) observation capture generation (checkedAdmissionDigest admission)
            0 [] (checkedTurnTemplates admission) (map exactModule (scopeValues scope)) (map programValueImport prefix)
            (case expressionPlanPresentation expression of ExpressionRendered -> "rendered"; ExpressionOpaque -> "opaque")
            (programOriginal state) prefix (scopeValues scope)
          localArgs = args { requestBindGen = Just generation, requestSessionRoot = Just root
            , requestInjectVals = map exactModule (scopeValues scope), requestRetainedGenerations = programRetained state }
          scoped :: Compiler
          scoped selection retained purpose session path includes products =
            compiler selection retained (CellProgramCompile (programPurpose state purpose) scope) session path includes products
          directory = outDir </> "display-" ++ show index
          verdict = StmtBinders KBind (checkedDisplayBinders display) []
      createDirectoryIfMissing True directory
      lastAttempt <- newIORef Nothing
      output <- timePhase timing "cell_program_display" $ compileClassifiedTurnKeeping scoped caches localArgs timing directory
        "" verdict (intercalate ", " (sbBinders verdict)) [] Nothing (Just display) lastAttempt (priorProgramImports state) Nothing
      extended <- retainProgramProducts (requestIncludes args) directory (compiledPipeline output)
        (compiledOriginalProducts output) (compiledModule output) scope
      let turn = compiledTurn output
      BS.writeFile (directory </> "turn.cbor") (encodeTurnOut turn)
      case turn of
        TBind _ _ binders _ wrapped -> do
          writeCheckedDisplayReceipt directory scope display (T.unpack wrapped)
          next <- addProgramValue root generation binders (state { programExact = extended })
          pure next { programValues = programValues state }
        _ -> fail "compiled cell display did not return bind metadata"

    scopeValues = maybe [] checkedValueInterfaces . scopeCheckedCell
    extractPlannedFingerprint inventory = plannedInterfaceFingerprint inventory
    plannedSourceFromDirectory plan = concatMap cellAnalysisSource (cellPlanItems plan)

programPurpose :: ProgramCellState -> CompilePurpose -> CompilePurpose
programPurpose state purpose = case purpose of
  GeneratedScaffoldCompile recipe inner -> GeneratedScaffoldCompile recipe (programPurpose state inner)
  OriginalDeclarationCompile -> ProgramItemCompile True [] (programOriginals state) (selectedProgramValues (programValues state))
  CheckedItemCompile annotations _ values -> ProgramItemCompile False annotations (programOriginals state) values
  other -> other

priorProgramImports :: ProgramCellState -> [String]
priorProgramImports state = [owner | ((_,owner),_) <- programOriginals state
  , Just owner /= fmap (snd . fst) (programOriginal state)]

renderProgramBinder :: String -> String
renderProgramBinder name
  | isSymOcc (mkVarOcc name) = "(" ++ name ++ ")"
  | otherwise = name

programValueImport :: CompletedValueImport -> (String,[String])
programValueImport value = (completedValueModule value,map fst (completedValueBinders value))

selectedProgramValues :: [CompletedValueImport] -> [CompletedValueImport]
selectedProgramValues values =
  [value { completedValueBinders = kept }
  | (index,value) <- zip [0::Int ..] values
  , let kept = [(name,identifier) | (name,identifier) <- completedValueBinders value
          , Map.lookup name winners == Just index]
  , not (null kept)]
  where winners = Map.fromList [(name,index) | (index,value) <- zip [0::Int ..] values
          , (name,_) <- completedValueBinders value]

installProgramImports :: [CompletedValueImport] -> [((String,String),String)] -> CellSourcePlan -> CellSourcePlan
installProgramImports values original plan = plan { cellPlanPrologue = prologue
  { prologueImports = prologueImports prologue ++ imports } }
  where
    prologue = cellPlanPrologue plan
    imports = [LocatedImport (CellSourceSpan 1 1 1 1) ("import " ++ owner)
      | ((_,owner),_) <- original]
      ++ [LocatedImport (CellSourceSpan 1 1 1 1) ("import " ++ completedValueModule value
        ++ " (" ++ intercalate ", " (map (renderProgramBinder . fst) (completedValueBinders value)) ++ ")") | value <- values]

globalProgramKeys :: Int -> Int -> String -> String
globalProgramKeys offset count source = T.unpack $ T.replace "__tidepool_program_" "__tidepool_cell_"
  (foldr replace (T.pack source) [0..count-1])
  where
    replace index = T.replace (T.pack ("__tidepool_cell_pin_" ++ show index ++ "_"))
        (T.pack ("__tidepool_program_pin_" ++ show (offset+index) ++ "_"))
      . T.replace (T.pack ("__tidepool_cell_expr_" ++ show index))
        (T.pack ("__tidepool_program_expr_" ++ show (offset+index)))
    -- Temporary prefixes avoid replacing a key twice when segments overlap.
    -- Normalize only after every local key has moved.

addProgramValue :: FilePath -> Word64 -> [BoundBinder] -> ProgramCellState -> IO ProgramCellState
addProgramValue _ _ [] state = pure state
addProgramValue root generation binders@(firstBinder:_) state = do
  owner <- maybe (fail "compiled Val owner is not canonical") pure
    (parseValModule ("Tidepool.Session.Val.G" ++ show generation))
  let path = sessionHiPath root owner
  bytes <- BS.readFile path
  packages <- BS.readFile (path ++ ".packages")
  requirementBytes <- BS.readFile (path ++ ".requirements")
  requirements <- case deserialiseFromBytes (do
      count <- CD.decodeListLen
      when (count > 4096) (fail "value type interface dependencies exceed bound")
      replicateM count $ do
        fields <- CD.decodeListLen
        unless (fields == 2) (fail "value type interface dependency is not a pair")
        (,) <$> (T.unpack <$> CD.decodeString) <*> (T.unpack <$> CD.decodeString)) (BL.fromStrict requirementBytes) of
    Right (remaining, owners) | BL.null remaining -> pure owners
    _ -> fail "value type interface dependencies are invalid"
  let digest = shaHex bytes
      artifact = ExactIfaceArtifact "main" (bbModule firstBinder) path digest requirements
      exact = programExact state
      admission = maybe (error "compiled cell lost admission") id (scopeCheckedCell exact)
      selection = CompletedValueImport "main" (bbModule firstBinder) path digest
        [(bbName binder,bbVarId binder) | binder <- binders, not ("__tidepoolMetadata" `isPrefixOf` bbName binder)]
  lexicalRequirements <- programLexicalRequirements exact [] requirements
  let
      extended = exact { scopeCheckedCell = Just admission
            { checkedValueInterfaces = checkedValueInterfaces admission ++ [artifact] }
        , scopeInterfaces = scopeInterfaces exact ++ [(artifact,path ++ ".packages",shaHex packages)]
        , scopeLexical = scopeLexical exact ++ [(("main",bbModule firstBinder),lexicalRequirements)] }
      retained = foldr (\binder -> Map.insert
          (SymbolIdentity "main" (T.pack (bbModule binder)) "value" (T.pack (bbName binder)) Nothing) generation)
        (programRetained state) binders
  pure state { programExact = extended, programValues = programValues state ++ [selection]
    , programRetained = retained }

-- Refine generated instances in the original declaration's typed environment.
-- The final prepared interface is the only original interface subsequently
-- installed in the checking transaction.
prepareOriginalCellDeclaration
  :: Compiler -> RecoveryCaches -> WorkerRequest -> String -> FilePath -> Maybe SessionScope
  -> ExactScope -> CheckedCellAdmission -> CellSourcePlan
  -> IO (CellSourcePlan, PlannedDeclaration, PlannedDeclarationInventory, ExactScope)
prepareOriginalCellDeclaration compiler caches args template outDir scope exact admission initial = do
  reserved <- case checkedReservedModules admission of
    [owner] -> pure owner
    _ -> throwIO InvalidDeclarationReservation
  wrapper <- originalDeclarationWrapper template
  let directory = outDir </> "planned-declaration"
      sourcePath = directory </> reserved ++ ".hs"
      planned plan = either rejectPlan pure (preparePlannedDeclaration reserved wrapper plan)
      writeOriginal plan = do
        original <- planned plan
        writeFile sourcePath (plannedSource original)
        writeFile (outDir </> "turn-attempt.hs") (plannedSource original)
        pure original
      checkOriginal plan = do
        _ <- writeOriginal plan
        compiler (maybe CheckedEnvironment CheckedEnvironmentProducts (requestModuleCandidates args)) Set.empty OriginalDeclarationCompile scope sourcePath
          (requestIncludes args) (requestBuildProductsDir args)
  createDirectoryIfMissing True directory
  (analyzed, provisional) <- checkCellInstances checkOriginal initial
  finalized <- if null (cellPlanDisplayTargets analyzed) then pure analyzed else do
    contexts <- cellDisplayDeclarations DisplayInstanceContexts provisional analyzed
    contextual <- pure (installCellDisplayDeclarations contexts analyzed)
    contextChecked <- checkOriginal contextual
    fields <- cellDisplayDeclarations DisplayInstanceFields contextChecked analyzed
    pure (installCellDisplayDeclarations fields analyzed)
  original <- writeOriginal finalized
  prepared <- compiler (PreparedProducts (requestModuleCandidates args)) (Map.keysSet (requestRetainedGenerations args))
    OriginalDeclarationCompile scope sourcePath (requestIncludes args) (requestBuildProductsDir args)
  let result = pprPipelineResult prepared
      environment = prHscEnv result
      binds = prBinds result
  inventory <- certifyPlannedDeclaration original environment >>= either fail pure
  originalInterfaces <- newOriginalInterfaceArtifacts environment (pprProductInterfaces prepared) directory
  (artifacts, productContext) <- prepareArtifacts originalInterfaces caches sourcePath environment (pprProductInterfaces prepared) (pprModules prepared)
    ["__result"] [] (requestRetainedGenerations args) (pprAcceptedCandidates prepared)
    (compilationScope <$> pprExactCompilation prepared)
  writePreparedSidecars SeparateYieldSites directory binds (prTyCons result)
    Nothing (map T.pack (prWarnings result)) artifacts
  writePreparedArtifacts directory artifacts
  certified <- writeCertifiedProductsKeeping originalInterfaces directory environment prepared productContext artifacts
  let products = certifiedOriginalProducts certified
  supportScope <- retainProgramProducts (requestIncludes args) directory prepared certified reserved exact
  originalProductEncoding <- case
      [product' | product' <- products
        , let (_, name, _, _) = moduleProductInput product', T.unpack name == reserved] of
    [value] -> pure value
    _ -> fail "planned original declaration has no unique prepared interface"
  let (unitText, _, interfaceBytes, originalGroups') = moduleProductInput originalProductEncoding
      unit = T.unpack unitText
      originalBytes = moduleProductBytes originalProductEncoding
      productPath = directory </> "original.product.cbor"
  BS.writeFile productPath originalBytes
  requirements <- programInterfaceRequirements prepared unit reserved
  lexicalRequirements <- programLexicalRequirements supportScope [] requirements
  let interfacePath = directory </> "original.hi"
      packagesPath = directory </> "original.hi.packages"
      interface = ExactIfaceArtifact unit reserved interfacePath (shaHex interfaceBytes) requirements
  roots <- maybe (fail "planned original declaration has no package interface witness") pure
    (Map.lookup (mkModuleName reserved) (pprPackageImports prepared))
  let packageBytes = encodePackageImports interface roots
      originalProduct = ExactProduct unit reserved
        (exactProgramProductVersion exact unit reserved (plannedSource original) interfaceBytes originalBytes packageBytes)
        (shaHex interfaceBytes) (shaHex originalBytes) productPath
        (map originalGroupFromProjected originalGroups')
      extended = supportScope
        { scopeProducts = scopeProducts supportScope ++ [originalProduct]
        , scopeInterfaces = scopeInterfaces supportScope ++ [(interface, packagesPath, shaHex packageBytes)]
        , scopeLexical = scopeLexical supportScope ++ [((unit,reserved), lexicalRequirements)] }
      text = encodeString . T.pack
      receipt = encodeListLen 8 <> text "TPEXACTDECL" <> text "1"
        <> text (scopeRequestSha256 exact) <> text reserved <> text (plannedSource original)
        <> text (shaHex interfaceBytes) <> text (renderPlannedDeclarationInventory inventory)
        <> text "planned-declaration"
  BS.writeFile interfacePath interfaceBytes
  BS.writeFile packagesPath packageBytes
  BS.writeFile (outDir </> "planned-declaration.cbor") (toStrictByteString receipt)
  pure (finalized, original, inventory, extended)
  where
    rejectPlan UnsupportedDeclarationOrder = throwIO (SourceRejection
      "local declarations currently require one initial group before bindings or expressions")
    rejectPlan ReservedResultDeclaration = throwIO (SourceRejection
      "authored declarations use the compiler-reserved __result binder")
    rejectPlan InvalidOriginalReservation = throwIO InvalidDeclarationReservation
    rejectPlan rejection = throwIO (InvalidDeclarationWrapper (show rejection))

-- Retain checked supporting originals in this cell's private scope. Cached
-- products keep their producer's bytes and version; fresh products use the
-- validated source witness. Neither path replaces an admitted hidden owner.
retainProgramProducts
  :: [FilePath] -> FilePath -> PreparedPipelineResult
  -> CertifiedOriginalProducts -> String -> ExactScope -> IO ExactScope
retainProgramProducts includes directory prepared certified target initial = do
  selected <- either fail pure (extendSourceSelectedOriginals
    (pprExactCompilation prepared >>= compilationSourceSelection) initial)
  cached <- foldM retainCached selected (zip [0::Int ..] (pprAcceptedCandidates prepared))
  promoted <- foldM retain cached (zip [0::Int ..] products)
  let parcels = mapMaybe candidateExecutionSources (pprAcceptedCandidates prepared)
  inherited <- either throwIO pure
    (extendExactExecutionSources (concatMap fst parcels) (map snd parcels) promoted)
  retainFreshExecutionSources includes prepared certified target inherited
  where
    products = [product' | product' <- certifiedOriginalProducts certified
      , let (_, owner, _, _) = moduleProductInput product', T.unpack owner /= target]
    supportOwners = [(candidateUnit candidate,candidateModule candidate)
      | candidate <- pprAcceptedCandidates prepared]
      ++ [(T.unpack unit,T.unpack owner) | product' <- products
          , let (unit,owner,_,_) = moduleProductInput product']
    retainCached scope (index, candidate) = do
      let unit = candidateUnit candidate
          owner = candidateModule candidate
          key = (unit,owner)
      interfaceBytes <- BS.readFile (candidateInterface candidate)
      packageBytes <- BS.readFile (candidatePackageImports candidate)
      productBytes <- BS.readFile (candidateProductPath candidate)
      unless (shaHex interfaceBytes == candidateInterfaceSha256 candidate
          && shaHex packageBytes == candidatePackageImportsSha256 candidate
          && shaHex productBytes == candidateProductSha256 candidate) $
        fail "accepted cached supporting original changed before retention"
      requirements <- programInterfaceRequirements prepared unit owner
      lexicalRequirements <- programLexicalRequirements scope supportOwners requirements
      let groups = map originalGroupFromCandidate (candidateGroups candidate)
          existingInterfaces = [(artifact,packages,sha)
            | (artifact,packages,sha) <- scopeInterfaces scope
            , (exactUnit artifact,exactModule artifact) == key]
          existingProducts = [original | original <- scopeProducts scope
            , (originalUnit original,originalModule original) == key]
      case (existingInterfaces,existingProducts) of
        ([],[]) -> do
          let stem = directory </> "retained-cached-original-" ++ show index
              interfacePath = stem ++ ".hi"
              packagesPath = stem ++ ".hi.packages"
              productPath = stem ++ ".product.cbor"
              interface = ExactIfaceArtifact unit owner interfacePath
                (candidateInterfaceSha256 candidate) requirements
              original = ExactProduct unit owner (candidateModuleVersion candidate)
                (candidateInterfaceSha256 candidate) (candidateProductSha256 candidate) productPath groups
          -- Keep the producer's original framing and module version. This
          -- private support entry cannot become a replacement source owner.
          BS.writeFile interfacePath interfaceBytes
          BS.writeFile packagesPath packageBytes
          BS.writeFile productPath productBytes
          pure scope { scopeProducts = scopeProducts scope ++ [original]
            , scopeInterfaces = scopeInterfaces scope ++ [(interface,packagesPath,candidatePackageImportsSha256 candidate)]
            , scopeLexical = scopeLexical scope ++ [(key,lexicalRequirements)] }
        ([(interface,packagesPath,packagesSha)],[original])
          | lookup key (scopeLexical scope) == Just lexicalRequirements
          , exactRequirements interface == requirements
          , exactSha256 interface == candidateInterfaceSha256 candidate
          , packagesSha == candidatePackageImportsSha256 candidate
          , originalVersion original == candidateModuleVersion candidate
          , originalIfaceSha256 original == candidateInterfaceSha256 candidate
          , originalProductSha256 original == candidateProductSha256 candidate
          , originalGroups original == groups -> do
              currentInterface <- BS.readFile (exactPath interface)
              currentPackages <- BS.readFile packagesPath
              currentProduct <- BS.readFile (originalProductPath original)
              unless (currentInterface == interfaceBytes && currentPackages == packageBytes
                  && currentProduct == productBytes) $
                fail "retained cached supporting original changed between cell slots"
              pure scope
        _ -> fail "cached source product conflicts with an admitted original owner"
    retain scope (index, originalProduct) = do
      let (unitText,ownerText,interfaceBytes,groups) = moduleProductInput originalProduct
          unit = T.unpack unitText
          owner = T.unpack ownerText
          admitted = [(exactUnit artifact,exactModule artifact) | (artifact,_,_) <- scopeInterfaces scope]
      when ((unit,owner) `elem` admitted) (fail "fresh source product replaces an admitted original owner")
      sourceDigest <- case
          [dependencySourceSha256 source | node <- dependencyModules (pprDependencies prepared)
            , dependencyModuleUnit node == unit, dependencyModuleName node == owner
            , source <- dependencySources (pprDependencies prepared)
            , dependencySourcePath source == dependencyModuleSource node] of
        [digest] -> pure digest
        _ -> fail "supporting original lacks one validated source witness"
      roots <- maybe (fail "supporting original lacks package interface witness") pure
        (Map.lookup (mkModuleName owner) (pprPackageImports prepared))
      requirements <- programInterfaceRequirements prepared unit owner
      lexicalRequirements <- programLexicalRequirements scope supportOwners requirements
      let stem = directory </> "retained-original-" ++ show index
          interfacePath = stem ++ ".hi"
          packagesPath = stem ++ ".hi.packages"
          productPath = stem ++ ".product.cbor"
          interface = ExactIfaceArtifact unit owner interfacePath (shaHex interfaceBytes) requirements
          packageBytes = encodePackageImports interface roots
          productBytes = moduleProductBytes originalProduct
          original = ExactProduct unit owner
            (exactProgramProductVersionFromDigest scope unit owner sourceDigest interfaceBytes productBytes packageBytes)
            (shaHex interfaceBytes) (shaHex productBytes) productPath
            (map originalGroupFromProjected groups)
      BS.writeFile interfacePath interfaceBytes
      BS.writeFile packagesPath packageBytes
      BS.writeFile productPath productBytes
      pure scope { scopeProducts = scopeProducts scope ++ [original]
        , scopeInterfaces = scopeInterfaces scope ++ [(interface,packagesPath,shaHex packageBytes)]
        , scopeLexical = scopeLexical scope ++ [((unit,owner),lexicalRequirements)] }

-- A later item can execute a quoter defined by an original retained here.
-- Keep its consumed source recipe at the same boundary as its native product,
-- rather than waiting for the frontend's postworker item certification.
retainFreshExecutionSources
  :: [FilePath] -> PreparedPipelineResult -> CertifiedOriginalProducts -> String
  -> ExactScope -> IO ExactScope
retainFreshExecutionSources includes prepared certified target scope
  | null supporting = pure scope
  | not (dependencyCacheSafe evidence && dependencySelectionComplete evidence) = pure scope
  | otherwise = do
      compilation <- maybe (fail "fresh supporting original has no compiler transaction") pure
        (pprExactCompilation prepared)
      let exactRows = compilationImports compilation
      if any (\((_,_,boot),edges) -> boot || any (\(_,_,isBoot,_) -> isBoot) edges) exactRows
        then pure scope
        else do
          origin <- normalise <$> makeAbsolute (compilationSource compilation)
          sourceBytes <- BS.readFile origin
          case [source | source <- dependencySources evidence, dependencySourcePath source == origin] of
            [source] | dependencySourceSha256 source == shaHex sourceBytes -> pure ()
            _ -> throwIO (ExecutionSourceChanged ("",target))
          validateDependencyEvidence evidence
          source <- either (const (throwIO (ExecutionSourceUnsupported ("",target))))
            (pure . T.unpack) (TE.decodeUtf8' sourceBytes)
          allRootsPresent <- and <$> mapM doesPathExist includes
          roots <- if allRootsPresent then mapM canonicalizePath includes else pure []
          fresh <- mapM freshIdentity fullProducts
          packages <- foldM retainPackage Map.empty
            (concatMap packageInterfaces (Map.elems (pprPackageImports prepared)))
          let freshKeys = Set.fromList (map executionIdentityKey fresh)
              prior = Map.fromList
                [(executionIdentityKey (executionRefIdentity ref),ref) | ref <- scopeExecutionOwners scope]
              originalIdentity product' = ExecutionSourceIdentity
                (originalUnit product') (originalModule product') (originalVersion product')
                (originalIfaceSha256 product') (originalProductSha256 product')
              inherited = [ExecutionSourceOwner original False
                  (executionRefGraph <$> Map.lookup (executionIdentityKey original) prior)
                | product' <- scopeProducts scope, let original = originalIdentity product'
                , executionIdentityKey original `Set.notMember` freshKeys]
              normalized = evidence
                { dependencySources = [row {dependencySourcePath = marker (dependencySourcePath row)}
                    | row <- dependencySources evidence]
                , dependencyModules = [node
                    { dependencyModuleSource = marker (dependencyModuleSource node)
                    , dependencyModuleImports = [edge
                        { dependencyImportSelected = marker <$> dependencyImportSelected edge }
                        | edge <- dependencyModuleImports node] }
                    | node <- dependencyModules evidence] }
              marker path | path == origin = "@generated-source"
                          | otherwise = path
              recipe = ExecutionSourceRecipe (scopeProducerSha256 scope) (Just (scopeSemanticSha256 scope))
                roots (origin,source) normalized
                (map snd (Map.toAscList (Map.fromList
                  [(executionIdentityKey (executionOwnerIdentity owner'),owner')
                  | owner' <- map (\original -> ExecutionSourceOwner original True Nothing) fresh ++ inherited])))
                [((unit,name), Set.toAscList (Set.fromList [(importedUnit,imported)
                    | (_,imported,False,importedUnit) <- edges]))
                  | ((unit,name,False),edges) <- exactRows]
                [(packageUnit root,packageModule root,packagePath root,packageSha256 root)
                  | root <- Map.elems packages]
          issued <- if allRootsPresent then either throwIO pure (issueExecutionSourceRecipe recipe)
            else pure Nothing
          case issued of
            Nothing -> pure scope
            Just graph -> do
              let availableGraphs = graph : scopeExecutionGraphs scope
                  prospective = [ExecutionSourceRef original (executionGraphSha256 graph)
                    | original <- fresh, executionModule original /= target]
              references <- either throwIO pure (executionSourceProspectiveReferences
                availableGraphs (scopeExecutionOwners scope) prospective)
              if null references then pure scope else do
                extended <- either throwIO pure
                  (extendExactExecutionSourcesWithinBudget [graph] references scope)
                pure (fromMaybe scope extended)
  where
    evidence = certifiedOriginalDependencies certified
    fullProducts = certifiedOriginalProducts certified
    supporting = [product' | product' <- fullProducts
      , let (_,owner,_,_) = moduleProductInput product', T.unpack owner /= target]
    freshIdentity product' = do
      let (unitText,ownerText,interfaceBytes,_) = moduleProductInput product'
          unit = T.unpack unitText
          owner = T.unpack ownerText
      sourceDigest <- case [dependencySourceSha256 source
          | node <- dependencyModules evidence
          , dependencyModuleUnit node == unit, dependencyModuleName node == owner
          , source <- dependencySources evidence
          , dependencySourcePath source == dependencyModuleSource node] of
        [sha] -> pure sha
        _ -> fail "fresh execution original lacks one consumed source witness"
      packages <- maybe (fail "fresh execution original lacks package witness") pure
        (Map.lookup (mkModuleName owner) (pprPackageImports prepared))
      let interface = ExactIfaceArtifact unit owner "" (shaHex interfaceBytes) []
          nativeBytes = moduleProductBytes product'
          packageBytes = encodePackageImports interface packages
          expected = ExecutionSourceIdentity unit owner
            (exactProgramProductVersionFromDigest scope unit owner sourceDigest interfaceBytes nativeBytes packageBytes)
            (shaHex interfaceBytes) (shaHex nativeBytes)
      case [ExecutionSourceIdentity unit owner (originalVersion original)
              (originalIfaceSha256 original) (originalProductSha256 original)
            | original <- scopeProducts scope
            , (originalUnit original,originalModule original) == (unit,owner)] of
        [original] | original == expected -> pure original
        [] | owner == target -> pure expected
        _ -> throwIO (ExecutionSourceConflicting (unit,owner))
    retainPackage selected root =
      let key = (packageUnit root,packageModule root)
      in case Map.lookup key selected of
        Nothing -> pure (Map.insert key root selected)
        Just previous | previous == root -> pure selected
        _ -> throwIO (ExecutionSourceConflicting key)

-- Interface requirements retain every exact hydration owner. Only selected
-- lexical owners contribute edges to the instance/family traversal graph.
programLexicalRequirements :: ExactScope -> [(String,String)] -> [(String,String)] -> IO [(String,String)]
programLexicalRequirements scope freshOwners requirements = do
  let interfaces = Set.fromList (freshOwners ++ [(exactUnit artifact,exactModule artifact)
        | (artifact,_,_) <- scopeInterfaces scope])
      lexical = Set.fromList (freshOwners ++ map fst (scopeLexical scope))
  unless (all (`Set.member` interfaces) requirements)
    (fail "program interface requirement leaves admitted exact owner closure")
  pure (filter (`Set.member` lexical) requirements)

programInterfaceRequirements :: PreparedPipelineResult -> String -> String -> IO [(String, String)]
programInterfaceRequirements prepared unit owner = do
  fresh <- either fail pure (selectedHomeRequirements (pprDependencies prepared) unit owner)
  let exact = [(importedUnit,name)
        | compilation <- maybe [] pure (pprExactCompilation prepared)
        , ((sourceUnit,sourceName,False), edges) <- compilationImports compilation
        , sourceUnit == unit, sourceName == owner
        , (_,name,False,importedUnit) <- edges]
  pure (nub (fresh ++ exact))

exactProgramProductVersion :: ExactScope -> String -> String -> String -> BS.ByteString -> BS.ByteString -> BS.ByteString -> String
exactProgramProductVersion scope unit owner source =
  exactProgramProductVersionFromDigest scope unit owner (shaHex (TE.encodeUtf8 (T.pack source)))

exactProgramProductVersionFromDigest :: ExactScope -> String -> String -> String -> BS.ByteString -> BS.ByteString -> BS.ByteString -> String
exactProgramProductVersionFromDigest scope unit owner sourceDigest iface productBytes packages = shaHex (BS.concat (map frame fields))
  where
    fields = ["tidepool-exact-source-home-v2", unhex (scopeProducerSha256 scope), unhex (scopeSemanticSha256 scope)
      , TE.encodeUtf8 (T.pack unit), TE.encodeUtf8 (T.pack owner)
      , unhex sourceDigest, iface, productBytes, packages]
    frame bytes = BS.pack [fromIntegral ((fromIntegral (BS.length bytes) :: Word64) `shiftR` shift)
      | shift <- [56,48..0]] <> bytes
    unhex [] = BS.empty
    unhex (first:second:rest) = case readHex [first,second] of
      [(byte,"")] -> BS.cons byte (unhex rest)
      _ -> error "admitted digest is not hexadecimal"
    unhex _ = error "admitted digest is not even length"

originalDeclarationWrapper :: String -> IO String
originalDeclarationWrapper template = do
  let marker = "\n__tidepoolInEffectRow ::"
      (prefix, remaining) = T.breakOn (T.pack marker) (T.pack template)
  when (T.null remaining) (fail "original declaration requires canonical whole-cell recipe")
  let stripped = T.replace "{{CELL_PRAGMAS}}" "" prefix
  when ("{{" `T.isInfixOf` T.replace "{{CELL_IMPORTS}}" "" stripped) (fail "original declaration wrapper has an unknown placeholder")
  pure (T.unpack stripped ++ "\n{{TURN}}\n__result :: Int\n__result = (0 :: Int)\n")

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

validateCheckedItemAdmission :: WorkerRequest -> CheckedItemAdmission -> String -> StmtBinders -> IO ()
validateCheckedItemAdmission args admission source verdict = do
  templates <- forM (requestTurnTemplates args) $ \(kind,path) -> (,) kind . shaHex <$> BS.readFile path
  generation <- requireArg "--bind-gen" (requestBindGen args)
  let expectedKind = case sbKind verdict of KBind -> "bind"; KExpr -> "expr"; KDecl -> "decl"
      keys = if expectedKind == "bind"
        then ["__tidepool_cell_pin_" ++ show (itemIndex admission) ++ "_" ++ binder | binder <- itemBinders admission]
        else ["__tidepool_cell_expr_" ++ show (itemIndex admission)]
  unless (shaHex (TE.encodeUtf8 (T.pack source)) == itemSourceDigest admission
      && expectedKind == itemKind admission && sbBinders verdict == itemBinders admission
      && generation == itemGeneration admission && requestInjectVals args == itemInjectedModules admission
      && templates == itemTurnTemplates admission && map signatureKey (itemSignatures admission) == keys)
    (fail "checked item body, verdict, generation, signatures or recipe differs from its protected offer")
  when ("__tidepool_checked_annotation_" `isInfixOf` source)
    (fail "authored checked item uses a compiler-reserved annotation name")

checkedDisplayBinders :: CheckedDisplayAdmission -> [String]
checkedDisplayBinders admission =
  ["__tidepoolPage" ++ show (displayGeneration admission)
  ,"__tidepoolMetadata" ++ show (displayGeneration admission),"cellDisplay"]

validateCheckedDisplayAdmission :: WorkerRequest -> CheckedDisplayAdmission -> String -> StmtBinders -> IO ()
validateCheckedDisplayAdmission args admission source verdict = do
  templates <- forM (requestTurnTemplates args) $ \(kind,path) -> (,) kind . shaHex <$> BS.readFile path
  generation <- requireArg "--bind-gen" (requestBindGen args)
  unless (null source && sbKind verdict == KBind && sbBinders verdict == checkedDisplayBinders admission
      && generation == displayGeneration admission && templates == displayTurnTemplates admission
      && requestInjectVals args == displayInjectedModules admission
      && not (requestActivationPreview args))
    (fail "display request differs from its completed observation admission")
  let observation = SymbolIdentity "main"
        (T.pack ("Tidepool.Session.Val.G" ++ show (displayCaptureGeneration admission)))
        "value" (T.pack (displayObservationName admission)) Nothing
  unless (Map.lookup observation (requestRetainedGenerations args) == Just (displayCaptureGeneration admission))
    (fail "display lacks its exact retained observation generation")

checkedDisplayRecipe :: CheckedDisplayAdmission -> String -> IO String
checkedDisplayRecipe = checkedDisplayRecipeWithInputs False

checkedProgramDisplayRecipe :: CheckedDisplayAdmission -> String -> IO String
checkedProgramDisplayRecipe = checkedDisplayRecipeWithInputs True

checkedDisplayRecipeWithInputs :: Bool -> CheckedDisplayAdmission -> String -> IO String
checkedDisplayRecipeWithInputs generic admission template = do
  let rowPrefix = "{{TURN_STMT}} ; _ <- (pure () :: Eff "
      rows = [suffix | line <- lines template, Just suffix <- [stripPrefix rowPrefix line]]
  effectRow <- case rows of
    [suffix] -> do
      case T.stripSuffix " ())" (T.pack suffix) of
        Just row | not (T.null row) -> pure (T.unpack row)
        _ -> fail "display requires the canonical bind effect-row pin"
    _ -> fail "display requires one exact bind effect-row pin"
  withImports <- replaceRecipeMarker "default (Int, Double, Text)\n"
    ("import qualified Tidepool.Inspection as TidepoolInspection\n"
      ++ (if generic then "import qualified " ++ show (unitString (moduleUnit (nameModule intTyConName)))
        ++ " " ++ moduleNameString (moduleName (nameModule intTyConName))
        ++ " as TidepoolProgramTypes\nimport qualified \"text\" Data.Text as TidepoolProgramText\n" else "")
      ++ concatMap (\(name, binders) -> "import " ++ name ++ " (" ++ intercalate ", " (map renderProgramBinder binders) ++ ")\n")
        (displayValueImports admission) ++ "default (Int, Double, Text)\n") template
  unless ("__result = do {\n{{TURN_STMT}}" `isInfixOf` withImports)
    (fail "display requires canonical bind recipe version one")
  (page,metadata,alias) <- case checkedDisplayBinders admission of
    [page,metadata,alias] -> pure (page,metadata,alias)
    _ -> fail "display requires its three canonical binders"
  let keys = intercalate "," ["T.pack " ++ show key | key <- displayPresented admission]
      budget = if generic then "(__tidepoolBudget :: TidepoolProgramTypes.Int)" else show (displayBudget admission)
      presented = if generic then "(__tidepoolPresented :: [TidepoolProgramText.Text])" else "[" ++ keys ++ "]"
      rendering = if displayPresentation admission == "rendered"
        then "TidepoolInspection.displayPageWithout " ++ presented ++ " " ++ budget
          ++ " (" ++ displayObservationName admission ++ " ())"
        else "TidepoolInspection.pageWithContinuation " ++ budget
          ++ " (TidepoolInspection.TextLeaf (T.pack \"<opaque value>\")) Nothing"
      statement = "(" ++ intercalate ", " [page,metadata,alias] ++ ") <- do {\n"
        ++ page ++ " <- pure ((" ++ rendering ++ ") :: TidepoolInspection.DisplayPage " ++ effectRow ++ ");\n"
        ++ metadata ++ " <- pure (T.copy (TidepoolInspection.text " ++ page ++ "), TidepoolInspection.pageHasMore "
        ++ page ++ ", TidepoolInspection.pageUnavailable " ++ page ++ ");\n"
        ++ alias ++ " <- pure " ++ page ++ ";\npure (" ++ intercalate ", " [page,metadata,alias] ++ ")\n}"
  let spliced = (if generic then ("{-# LANGUAGE PackageImports, ScopedTypeVariables #-}\n" ++) else id) (spliceTemplate withImports statement (intercalate ", " [page,metadata,alias]))
  if generic then do
    withArgument <- replaceRecipeMarker "__result = do {" "__result ((__tidepoolBudget :: TidepoolProgramTypes.Int), (__tidepoolPresented :: [TidepoolProgramText.Text])) = do {" spliced
    prepared <- replaceRecipeMarker "__prepared = TidepoolResume.settle __result" "__prepared input = TidepoolResume.settle (__result input)" withArgument
    pure prepared
  else pure spliced

writeCheckedDisplayReceipt :: FilePath -> ExactScope -> CheckedDisplayAdmission -> String -> IO ()
writeCheckedDisplayReceipt root scope admission source = do
  let text = encodeString . T.pack
      receipt = encodeListLen 8 <> text "TPEXACTDISPLAY" <> text "1"
        <> text (scopeRequestSha256 scope) <> text (displayCellReceiptDigest admission)
        <> encodeWord64 (displayItemIndex admission) <> text (displayPrefixDigest admission)
        <> text (shaHex (TE.encodeUtf8 (T.pack source))) <> text "tidepool-display-recipe-1"
  BS.writeFile (root </> "checked-display.cbor") (toStrictByteString receipt)

checkedRecipeAnnotations :: CheckedItemAdmission -> [(String,CheckedSignature)]
checkedRecipeAnnotations admission =
  [("__tidepool_checked_annotation_" ++ show index,signature)
  | (index,signature) <- zip [(0::Int)..] (itemSignatures admission)]

checkedItemCompilePurpose :: CheckedItemAdmission -> CompilePurpose
checkedItemCompilePurpose admission = case itemPurpose admission of
  AuthoredCheckedItem -> CheckedItemCompile annotations original values
  HostActivationInput -> HostActivationInputCompile annotations original values
  where
    annotations = checkedRecipeAnnotations admission
    original = itemPlannedDeclaration admission
    values = itemCompletedValues admission

-- The admitted template is a versioned recipe input. Transform its markers
-- before inserting authored bytes, so authored syntax is never rescanned.
checkedRecipeSource :: CheckedItemAdmission -> String -> String -> IO String
checkedRecipeSource admission template source = case itemKind admission of
  "bind" -> do
    unless ("__result = do {\n{{TURN_STMT}}" `isInfixOf` template)
      (fail "checked bind requires canonical recipe version one")
    let aliases = checkedRecipeAnnotations admission
        declarations = intercalate "; " [alias ++ " :: (" ++ signatureType signature ++ "); "
          ++ alias ++ " = " ++ binder | ((alias,signature),binder) <- zip aliases (itemBinders admission)]
        result = intercalate ", " (map fst aliases)
    amended <- if null aliases then pure template else replaceRecipeMarker "{{TURN_STMT}}"
      ("{{TURN_STMT}}\n; let { " ++ declarations ++ " }\n") template
    pure (spliceTemplate amended source result)
  "expr" -> case checkedRecipeAnnotations admission of
    [(alias,signature)] -> do
      observation <- maybe (fail "checked expression has no owning observation name") pure (itemObservationName admission)
      unless ("__result = do {\n{{TURN_STMT}}" `isInfixOf` template)
        (fail "checked expression capture requires canonical bind recipe version two")
      let liftStatement = case itemExpressionLift admission of
            Just "effectful" -> "__tidepool_checked_captured_value <- " ++ alias
              ++ "\n; let { " ++ observation ++ " = (\\() -> __tidepool_checked_captured_value) }"
            Just "pure" -> "let { " ++ observation ++ " = (\\() -> " ++ alias ++ ") }"
            _ -> ""
      when (null liftStatement) (fail "checked expression has no certified lift plan")
      let statement = "let { " ++ alias ++ " :: (" ++ signatureType signature ++ "); "
            ++ alias ++ " = (\n" ++ source ++ "\n) }\n; " ++ liftStatement
      pure (spliceTemplate template statement observation)
    _ -> fail "checked expression has no unique full signature"
  _ -> fail "checked declaration lacks an original identity certificate"

replaceRecipeMarker :: String -> String -> String -> IO String
replaceRecipeMarker marker replacement template =
  either fail pure (replaceTemplateMarker marker replacement template)

writeCheckedItemReceipt :: FilePath -> ExactScope -> CheckedItemAdmission -> String -> IO ()
writeCheckedItemReceipt root scope admission source = do
  let (file,magic,profile) = case itemPurpose admission of
        AuthoredCheckedItem -> ("checked-item.cbor","TPEXACTITEM","tidepool-checked-recipe-2")
        HostActivationInput -> ("activation-input.cbor","TPEXACTACTIVATIONINPUT2","tidepool-host-activation-input-2")
      text = encodeString . T.pack
      receipt = encodeListLen (if itemPurpose admission == HostActivationInput then 9 else 8) <> text magic
        <> text (if itemPurpose admission == HostActivationInput then "2" else "1")
        <> text (scopeRequestSha256 scope) <> text (itemAdmissionDigest admission)
        <> text (itemCellReceiptDigest admission) <> encodeWord64 (itemIndex admission)
        <> text (shaHex (TE.encodeUtf8 (T.pack source))) <> text profile
  witness <- case itemPurpose admission of
    AuthoredCheckedItem -> pure mempty
    HostActivationInput -> encodeBytes <$> BS.readFile (root </> "activation-type.cbor")
  BS.writeFile (root </> file) (toStrictByteString (receipt <> witness))

-- Fold eligibility and attempted compilation are separate from whole-cell
-- checking. An expression or declaration is a successful ineligible outcome.
data CellFoldIneligibility
  = FoldEmptyCell | FoldMultipleItems | FoldExpressionItem | FoldDeclarationItem

data CellFoldStage = FoldCheckingEvidence | FoldCompilingTurn | FoldSealingReceipt

data CellFoldOutcome
  = CellFoldNotRequested
  | CellFoldIneligible CellFoldIneligibility
  | CellFoldAttemptedFailed CellFoldStage String
  | CellFoldCompiled

encodeCellFoldOutcome :: CellFoldOutcome -> BS.ByteString
encodeCellFoldOutcome outcome = toStrictByteString $
  encodeListLen 5 <> text "TPCELLFOLD" <> encodeWord 1 <> case outcome of
    CellFoldNotRequested -> text "not-requested" <> encodeNull <> encodeNull
    CellFoldIneligible reason -> text "ineligible" <> text (case reason of
      FoldEmptyCell -> "empty-cell"; FoldMultipleItems -> "multiple-items"
      FoldExpressionItem -> "expression"; FoldDeclarationItem -> "declaration") <> encodeNull
    CellFoldAttemptedFailed stage cause -> text "attempted-failed" <> text (case stage of
      FoldCheckingEvidence -> "checking-evidence"; FoldCompilingTurn -> "compiling-turn"
      FoldSealingReceipt -> "sealing-receipt") <> text cause
    CellFoldCompiled -> text "compiled" <> encodeNull <> encodeNull
  where text = encodeString . T.pack

-- A synchronous attempted failure preserves the successful whole-cell result.
-- Asynchronous cancellation propagates through the existing worker boundary.
attemptCellFoldTurn
  :: Compiler -> RecoveryCaches -> WorkerRequest -> Bool -> FilePath
  -> CellSourcePlan -> CheckedEnvironmentResult -> Maybe ExactScope -> IO CellFoldOutcome
attemptCellFoldTurn compiler caches args timing outDir finalPlan compiled admittedScope =
  case soleBindItem finalPlan of
    Nothing -> pure (CellFoldIneligible (case cellPlanItems finalPlan of
      [] -> FoldEmptyCell
      [item] -> case sbKind (cellAnalysisVerdict item) of
        KExpr -> FoldExpressionItem; KDecl -> FoldDeclarationItem
        KBind -> FoldMultipleItems
      _ -> FoldMultipleItems))
    Just item -> do
      stage <- newIORef FoldCheckingEvidence
      result <- trySynchronous (attempt stage item)
      case result of
        Right () -> pure CellFoldCompiled
        Left exception -> do
          failedStage <- readIORef stage
          pure (CellFoldAttemptedFailed failedStage (take 2048 (show exception)))
  where
    attempt stage item = do
        outFile <- requireArg "--turn-out" (requestTurnOut args)
        let sb = cellAnalysisVerdict item
            turnSrc = cellAnalysisSource item
            pins = itemBinderPins 0 (sbBinders sb) (crCheckedBinderPins compiled)
        bindersStr <- either fail pure (renderPinnedBinders (sbBinders sb) pins)
        lastAttempt <- newIORef Nothing
        writeIORef stage FoldCheckingEvidence
        admitted <- case admittedScope >>= scopeCheckedCell of
          Nothing -> pure Nothing
          Just cellAdmission -> do
            inventory <- cellCheckedBinderSignatures compiled
            signatures <- forM (sbBinders sb) $ \binder -> case
                [signature | signature <- inventory, signatureKey signature == "__tidepool_cell_pin_0_" ++ binder] of
              [signature] -> pure signature
              _ -> fail "fold has no unique binder signature authority"
            receipt <- BS.readFile (outDir </> "checked-cell.cbor")
            generation <- requireArg "--bind-gen" (requestBindGen args)
            let admission = CheckedItemAdmission AuthoredCheckedItem (checkedAdmissionDigest cellAdmission) (shaHex receipt) 0
                  (shaHex (TE.encodeUtf8 (T.pack turnSrc))) "bind" (sbBinders sb)
                  (checkedTurnTemplates cellAdmission) (checkedInjectedModules cellAdmission)
                  signatures Nothing Nothing generation (checkedAdmissionDigest cellAdmission) [] Nothing Nothing [] (checkedValueInterfaces cellAdmission)
            validateCheckedItemAdmission args admission turnSrc sb
            pure (Just admission)
        let typeImports = if isJust admitted then [] else nub (concatMap checkedPinImports [pin | Just pin <- pins])
        writeIORef stage FoldCompilingTurn
        turnOut <- compileClassifiedTurn compiler caches args timing outDir turnSrc sb bindersStr typeImports admitted Nothing lastAttempt
        writeIORef stage FoldSealingReceipt
        BS.writeFile outFile (encodeTurnOut turnOut)
        forM_ (admittedScope >>= \scope -> (,) scope <$> admitted) $ \(scope,admission) ->
          case turnOut of
            TBind _ _ _ _ wrapped -> writeCheckedItemReceipt outDir scope admission (T.unpack wrapped)
            _ -> fail "checked fold did not produce its bind recipe"

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
-- 'StmtBinders' shape 'classifyWithFlags' would have produced, so the rest of
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
