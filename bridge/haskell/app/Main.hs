{-# LANGUAGE RankNTypes #-}

module Main where

import System.Environment (getArgs)
import System.FilePath (takeBaseName, takeDirectory, (</>))
import System.Directory (createDirectoryIfMissing, setCurrentDirectory)
import qualified Data.ByteString as BS
import qualified Data.ByteString.Lazy as BL
import qualified Codec.CBOR.Decoding as CD
import Codec.CBOR.Read (deserialiseFromBytes)
import Codec.CBOR.Encoding (Encoding, encodeBytes, encodeListLen, encodeString, encodeWord64, encodeWord32, encodeNull)
import Codec.CBOR.Write (toStrictByteString)
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import Control.Exception
  ( evaluate, try, throwIO, SomeException, Exception
  , toException, IOException, bracket )
import Data.IORef (IORef, newIORef, readIORef, writeIORef, modifyIORef')
import Data.List (intercalate, nub, isPrefixOf)
import Data.Maybe (fromMaybe, mapMaybe, isJust, isNothing)
import Data.Word (Word32, Word64)
import Control.Monad (replicateM, foldM, forM, forM_, when, unless, void)
import System.Exit (ExitCode(..), exitWith)
import System.IO (hPutStrLn, stderr, stdin, stdout, hSetBinaryMode, hSetEncoding, utf8, IOMode(ReadMode), withBinaryFile)

import GHC (moduleName, moduleNameString, moduleUnit, mkModuleName, mkModule)
import GHC.Driver.Session (DynFlags)
import GHC.Driver.Env (hsc_dflags)
import GHC.Driver.Ppr (showSDoc)
import GHC.Utils.Outputable (ppr)
import GHC.Tc.Types (tcg_mod)
import GHC.Unit.Module.ModIface (mi_module)
import GHC.Unit.Types (unitString, stringToUnit)
import GHC.Core (Bind(..), CoreBind)
import GHC.Core.DataCon (DataCon)
import GHC.Core.TyCon (TyCon)
import GHC.Types.Name (nameOccName)
import GHC.Types.Id (idName)
import GHC.Types.Name.Occurrence (occNameString)
import qualified Data.Text as T
import qualified Data.Text.Encoding as TE

import Tidepool.HarnessSource (spliceHarnessProfilePragma)

import Tidepool.Binders
  ( extractBindersNamedGhc
  , classifyWithFlags, exportItemName, templateParserFlags
  , analyzeCellWithFlags, analyzeOrderedCellWithFlags, cellInferenceSegments
  , renderCellCheckSource, renderCellCheckSourceWithLineOffset, CellSourceSpan(..)
  , PreparedTypedSegmentSource(..), prepareTypedSegmentSource
  , CellSourcePlan(..), CellAnalysisItem(..), CellBindingForm(..), CellExpressionPlan(..), BoundBinder(..),
    SourcePrologue(..), LocatedPragma(..), LocatedImport(..), ImportIntent(..), ExpressionLiftPlan(..)
  , declarationSourceWithTemplateFlags, renderDeclarationForTemplate
  , TurnKind(..), parseTurnKind
  , TemplateSelector(..), templateSelectorForVerdict, templateSelectorWireName
  , StmtBinders(..), TurnOut(..), renderAskJson, renderVerdictsJson
  )
import Tidepool.TypedSegment
  ( TypedSegmentPlan, typedSegmentPlanRoot, typedSegmentPlanItems, typedSegmentPlanDigest, typedSegmentReservationDigest
  , TypedItemPlan(..), TypedItemBody(..), typedSegmentOriginalRoot, typedSegmentItems, typedItemPlan, typedItemRoot
  , typedItemCaptures, typedCaptureIdentifier, typedCaptureType, typedItemObservation
  , typedObservationLift, typedObservationValueType, ObservationLift(..) )
import Tidepool.CompilerExecution (CompilerExecutor, compilerExecutionGrant, serialCompilerExecutionGrant, withCompilerExecutor)
import GHC.Conc (getNumCapabilities, setNumCapabilities)
import Tidepool.GhcPipeline
  ( PipelineSelection(..), PreparedPipelineResult(..), PreparedSegmentProductsResult(..), CheckedEnvironmentResult(..)
  , preparedFreshDependencies, preparedExactCompilation, preparedHomeRequirements
  , retainProgramSourceImports, withProgramSourceImports
  , CompilePurpose(..), withSourceImportIntents, PipelineResult(..)
  , CompilerScope(..), CompilerRecoveryCaches(..), withResidentCompilerScopes, withScopedExactInterfaceTransaction
  , checkCellInstances, cellGeneratedInstanceRecipe
  , cellExpressionEvidence, cellCheckedBinderSignatures
  , satisfiesCapturedConstraint, activationPreviewInputType )
import Tidepool.ExecutionEncode (encodeWireProgram, moduleProductInput, moduleProductBytes)
import Tidepool.CompilerProducts
  ( CertifiedOriginalProducts, certifiedOriginalProducts, certifiedFinalizedArtifacts, certifiedSourceOriginals, certifiedExecutionSource
  , certifiedRetainedOriginals, certifiedRetainedNativeVersions, PreparedProductContext, prepareOriginalProductsWithCache
  , OriginalProductWorklist, observeOriginalProjectionWithRecovery, prepareOriginalProductsWithWorklist
  , requireOriginalExecutableGlobals, admitCurrentOriginalProducts, preparedCurrentOriginalInventory
  , preparedProductInventory, currentOriginalBinders, currentOriginalBindingsExcept, currentReconciledOriginalProducts
  , newPreparedOriginalInterfaceArtifacts, writeCertifiedProductsKeepingWithOriginals
  , writeCertifiedSegmentProducts, writeCertifiedSegmentItemProducts, prepareCompilerProjectionContext
  , exactProgramProductVersionFromDigest )
import Tidepool.ExecutionProjection (ProjectionContext(..), ProjectionError(..), prepareComponentProjectionWithReachability, projectSelectedCandidateWithHostBindings, candidateGlobals, finalizePreparedCandidate, preparedModuleProductOutcomes, preparedModuleProductConstructors, preparedModuleProductYieldSites, preparedRootIdentity)
import Tidepool.HostBindingAuthority
  ( HostBindingRepresentation, hostBindingRepresentationJsonAuthority )
import Tidepool.ExecutionSchema
  ( SymbolIdentity(..)
  , WireProgram(..), SiteRow(..) )
import qualified Tidepool.ExecutionSchema as Execution
import qualified Tidepool.EffectSchema
import Tidepool.PreparedStg
  ( pmModule, pmYieldSites )
import Tidepool.PreparedRecovery
  ( RecoveryFailure, RecoveredClosure(..), newPreparedRecoveryWithDemand
  , preparedRecoveryClosure, requirePreparedRecoveryPublication )
import Tidepool.ModuleCandidates
  ( ModuleCandidate(..), CandidateGroup(..), candidateExecutionSources )
import Tidepool.CompileInput (writeCompileInputProof)
import Tidepool.CertifiedProducts (resolvePackageGlobal, homeInterfaceUsageOwners)
import Tidepool.FinalizedModuleArtifacts
  ( finalizedLocalAdmissions
  , localFinalizedInterface, localFinalizedSourceSha256, localFinalizedCore )
import Tidepool.OriginalProductRoots (requiredOriginalPackageGlobalsWithRetained, unrecoveredExactProducts)
import Tidepool.ExactHydration (ExactIfaceArtifact(..), CheckedTemplateImports(..), OriginalInterfaceArtifacts
  , generatedActivationPreviewRecipe
  , RenderedProtectedTemplateImports
  , captureProtectedTemplateImports, captureProtectedTemplateImportsAt, renderProtectedTemplateImports
  , generatedScaffoldRecipeWithProtectedImports, generatedCheckingTemplateRecipeWithProtectedImports
  , generatedTypedSegmentRecipeWithProtectedImports)
import Tidepool.ExecutionSource
  ( ExecutionSourceGraph(..), ExecutionSourceIdentity(..)
  , ExecutionSourceOwner(..), ExecutionSourceRef(..), WorkerExecutionSource(..)
  , executionSourceProspectiveReferences )
import Tidepool.DeclarationJoin
  ( DeclarationOperation(..), readDeclarationOperation, validateDeclarationJoin
  , HostBindingInterfaceInput(..), encodeHostBindingInterface, encodeBindingInterfacePurpose
  , DeclarationExport(..), ExportIdentity(..), ExportNamespace(..)
  , renderDeclarationJoinOutcome, inspectDeclarationArtifacts
  , renderDeclarationInventoryOutcome )
import qualified Tidepool.WorkerServer as WorkerServer
import Tidepool.DiagJson
  ( SourceRejection(..), InputRejection(..) )
import Tidepool.CheckedAdmission
  ( matchesInspectionAdmission, validateCheckedCellAdmission, validateCheckedItemAdmission )
import Tidepool.CheckedRecipe
  ( checkedItemCompilePurpose, checkedRecipeSourceWithLineOffset, replaceRecipeMarker, writeCheckedItemReceipt )
import Tidepool.ExtractUtil (shaHex, trySynchronous)
import Tidepool.WorkerDiagnostics
  ( throwCellSplitError, sourceFailureDiagnostics, reportDiags, reportDiagsWithWarnings )
import Tidepool.ExtractRequest (RequestShapeError(..), validateRequestShape, WorkerRequest(..), workerRequestFromArgv, workerRequestFlag)
import Tidepool.Introspection (encodeInspectionResults, runInspectionGhc)
import Tidepool.InspectionRunner (isInspectionTypeQuery, runInspectionRequests)
import Tidepool.ExactScope
  ( ExactCompilation(..), ExactScope , scopeRequestSha256, scopeProducerSha256, scopeSemanticSha256, scopeLexical, scopeProducts, scopeExecutionGraphs, scopeExecutionOwners, scopePurpose, ExactScopePurpose(..), ExactProduct(..), ExactOriginalGroup(..)
  , ActivationPreviewAdmission(..), scopeActivationPreview
  , scopeCheckedCell, scopeCheckedItem, scopeIncludePaths
  , originalGroupFromProjected, originalGroupFromCandidate
  , CheckedCellAdmission(..), CheckedItemAdmission(..), CheckedItemPurpose(..), PlannedCellAdmission(..), PlannedCellSlot(..)
  , ExactInterfaceEvidence(..), validateCandidateCanonicalInterfaceProof, canonicalCertificateSha256, canonicalSourceSha256
  , readExactScope, revalidateExactScope, scopeInterfaces, scopeInterfaceEvidence, extendExactScopeGeneration, extendCheckedValueScope
  , scopeAvailableOriginalProducts
  , extendSourceSelectedOriginals, extendExactExecutionSources, extendExactExecutionSourcesWithinBudget )
import Tidepool.CheckedPrefixImports (CompletedValueImport(..))
import Tidepool.CellProgramState
import Tidepool.CheckedCell (encodeCheckedSignature, captureCheckedSignature
  , captureCheckedTypeWitness, sealCheckedTypeWitness, encodeCheckedTypeWitness
  , validateOriginalInputTypeWitness)
import Tidepool.TypePolicy (nominalHeadsOfType)
import Tidepool.PlannedDeclaration
  ( PlannedDeclaration, PlannedDeclarationRejection(..), PlannedDeclarationInventory, plannedExports, plannedSource, plannedCheckPlan, replaceTemplateModuleHeader
  , preparePlannedDeclaration, certifyPlannedDeclaration, admitLocalNativeDeclaration
  , renderPlannedDeclarationInventory, plannedInterfaceFingerprint )
import Tidepool.Session
  ( SessionScope(..), preparedScaffoldTargetName, preparedResumeTargetName
  , SessionModule(..), SessionModuleKind(..), Generation(..)
  , preparedApplyEntryTargetName, preparedApplyValueTargetName
  , sessionHiPath, sessionModuleString )
import Tidepool.FatIface
  ( readExactInterface )
import Tidepool.SessionArtifacts
  ( prepareSessionBindings, sessionBindingRepresentations, writeSessionBindings, parseValModule
  , emitHostBindingInterface
  , prepareTypedSegmentSessionBindings
  , typedSegmentSessionEnvironment, typedSegmentSessionGlobals, typedSegmentSessionInterfaces
  , typedSegmentSessionBinders, typedSegmentSessionRetainedGlobals
  , typedSegmentSessionBindingRepresentations, withTypedSegmentSessionPublication )
import Tidepool.Metadata (metadataForConstructors, targetBindingHasIO)
import Tidepool.CborEncode (encodeMetadata, encodeTurnOut, encodeCellOut, encodeBoundBinder)
import Tidepool.Timing (readTimingEnabled, timePhase, emitCount)
import Tidepool.TurnSource
  ( extractModuleName, spliceTemplate, renderImportBinder
  , generatedScaffoldModuleName, renameScaffoldModuleHeader
  , captureCompilerDefaultRecipe, qualifyCompilerDefaultWithLineOffset, preambleImportMarker )
import Tidepool.DependencyEvidence
  ( validateDependencyEvidence )

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
-- with an immutable withholding policy installed in each owning compiler view.
type Compiler =
  forall result. PipelineSelection result
  -> Set.Set SymbolIdentity
  -> CompilePurpose
  -> Maybe SessionScope
  -> FilePath
  -> [FilePath]
  -> Maybe FilePath
  -> IO result

-- Recovery values share their completed resolution context's lifetime.
-- Acquire after compilation, when the current context has been selected.
data RecoveryCaches = RecoveryCaches
  { acquireRecoveryCaches :: IO CompilerRecoveryCaches
  , recoveryExecutor :: Maybe CompilerExecutor
  , recoveryOriginalWorklist :: IO (Maybe OriginalProductWorklist)
  }

scopeRecoveryCaches :: CompilerScope -> RecoveryCaches
scopeRecoveryCaches scope = RecoveryCaches (scopedRecoveryCaches scope) (scopedExecutor scope) (pure Nothing)

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
      withResidentCompilerScopes [] $ \runRequest ->
        WorkerServer.runWorkerLoop $ \serveTransaction ->
          runRequest (pure ()) $ \scope ->
            serveTransaction (\cwd argv ->
              setCurrentDirectory cwd >> runWorkerInvocation scope (scopeRecoveryCaches scope) argv)
    else do
      hSetEncoding stdout utf8
      -- Direct transport boots the same compiler owner once for all helpers.
      result <- withResidentCompilerScopes [] $ \runRequest ->
        runRequest (pure ()) $ \scope ->
          runWorkerInvocation scope (scopeRecoveryCaches scope) rawWorkerRequest
      exitWith result

-- | Decode a Rust worker request and run one compilation. Direct and daemon transports use
-- the same versioned payload and therefore the same dispatch path.
runWorkerInvocation
  :: CompilerScope -> RecoveryCaches -> [String] -> IO ExitCode
runWorkerInvocation compilerScope caches rawWorkerRequest = do
  parsedWorkerRequest <- case workerRequestFromArgv rawWorkerRequest of
    Left err -> hPutStrLn stderr err >> pure Nothing
    Right (Just request) -> pure (Just request)
    Right Nothing -> hPutStrLn stderr "worker requires a versioned request" >> pure Nothing
  case parsedWorkerRequest of
    Nothing -> pure (ExitFailure 2)
    Just request -> runParsedInvocation compilerScope caches request

runParsedInvocation
  :: CompilerScope -> RecoveryCaches -> WorkerRequest -> IO ExitCode
runParsedInvocation compilerScope caches parsedWorkerRequest = do
  grant <- either fail pure (compilerExecutionGrant (requestCompilerJobs parsedWorkerRequest))
  bracket getNumCapabilities setNumCapabilities $ \_previous -> do
    setNumCapabilities (requestCompilerCapabilities parsedWorkerRequest)
    withCompilerExecutor grant $ \executor -> do
      originalWorklist <- newIORef Nothing
      let completion env interfaces owner exact inputs = do
            recovery <- scopedRecoveryCaches compilerScope
            (observer,worklist) <- observeOriginalProjectionWithRecovery
              (compilerOriginalProjections recovery) (compilerPreparedBodies recovery) executor
              (requestRetainedGenerations parsedWorkerRequest)
              [preparedResumeTargetName,preparedApplyEntryTargetName,preparedApplyValueTargetName] Nothing
              env interfaces owner exact inputs
            writeIORef originalWorklist (Just worklist)
            pure observer
          observeProducts :: PipelineSelection result -> PipelineSelection result
          observeProducts selection = case selection of
            PreparedStg -> WithPreparedModuleCompletion completion selection
            PreparedProducts _ -> WithPreparedModuleCompletion completion selection
            _ -> selection
          scope = compilerScope
            { scopedCompile = \selection retained purpose session source includes products -> do
                writeIORef originalWorklist Nothing
                scopedCompile compilerScope
                  (WithCompilerExecution grant executor (observeProducts selection))
                  retained purpose session source includes products
            , scopedExecutor = Just executor
            }
      runGrantedInvocation scope (caches
        {recoveryExecutor=Just executor,recoveryOriginalWorklist=readIORef originalWorklist}) parsedWorkerRequest

runGrantedInvocation
  :: CompilerScope -> RecoveryCaches -> WorkerRequest -> IO ExitCode
runGrantedInvocation compilerScope caches parsedWorkerRequest = do
  -- Read once per invocation (see Tidepool.Timing) and thread down;
  -- TIDEPOOL_TIMING is diagnostic-only and never touches stdout/the emitted
  -- files — see the module doc there.
  timing <- readTimingEnabled
  -- Apply the harness language profile by rewriting a scratch copy:
  -- rewrite the target to a pragma-prepended scratch copy BEFORE any mode
  -- dispatch below, so every mode (one-shot, session, turn) sees a plain
  -- file with no pragma-block requirement of its own. See
  -- 'spliceHarnessProfilePragma'.
  args <- if requestHarnessProfile parsedWorkerRequest && not (requestCheckSource parsedWorkerRequest)
            then spliceHarnessProfilePragma parsedWorkerRequest
            else pure parsedWorkerRequest
  dispatch compilerScope caches timing args

-- | Dispatch one decoded worker request.
-- The request owner decodes the offered metadata once. Compiler stages still
-- authenticate its current bytes and resolution before consuming it.
newtype AdmittedRequest = AdmittedRequest { admittedRequestScope :: Maybe ExactScope }

dispatch
  :: CompilerScope -> RecoveryCaches -> Bool -> WorkerRequest -> IO ExitCode
dispatch compilerScope caches timing args = do
  admitted <- trySynchronous $ do
    case validateRequestShape args of
      Left InvalidSourceCheckShape -> fail "source checking cannot carry product or notebook authority"
      Left InvalidCellPlanShape -> throwIO InvalidCellPlanRequest
      Right () -> pure ()
    exact <- forM (requestSessionArtifacts args) $ \manifest -> do
      scope <- readExactScope manifest >>= either fail pure
      forM_ (scopeIncludePaths scope) $ \includes ->
        unless (requestIncludes args == includes)
          (throwIO SearchInputsChanged)
      case scopePurpose scope of
        NoCheckedPurpose -> pure ()
        ExactInspectionPurpose values _ ->
          unless (matchesInspectionAdmission args (map exactModule values))
            (throwIO CheckedPurposeMismatch)
        ExactReloadInspectionPurpose values _ ->
          unless (matchesInspectionAdmission args (map exactModule values))
            (throwIO CheckedPurposeMismatch)
        ExactCellPurpose _ _ ->
          unless (requestCell args && not (requestTurn args) && not (requestClassify args)
            && null (requestInspections args) && not (isJust (requestDeclarationJoin args))
            && not (requestCertifyHomeProducts args) && not (requestActivationPreview args))
            (throwIO CheckedPurposeMismatch)
        ExactItemPurpose admission _ -> do
          let previewPurpose = case itemPurpose admission of
                AuthoredCheckedItem -> not (requestActivationPreview args)
          unless (requestTurn args && not (requestCell args) && not (requestClassify args)
            && null (requestInspections args) && not (isJust (requestDeclarationJoin args))
            && not (requestCertifyHomeProducts args) && previewPurpose
            && not (isJust (requestTarget args)))
            (throwIO CheckedPurposeMismatch)
        ExactActivationPreviewPurpose _ _ ->
          unless (requestActivationPreview args && requestTurn args
            && length (requestFiles args) == 1 && null (requestTurnTemplates args)
            && not (isJust (requestTurnVerdict args)) && not (requestCell args)
            && not (requestCellPlan args) && not (requestCheckSource args)
            && not (requestClassify args) && null (requestInspections args)
            && not (isJust (requestDeclarationJoin args)) && not (requestCertifyHomeProducts args)
            && not (requestHarnessProfile args) && null (requestTargets args)
            && not (isJust (requestTarget args)) && null (requestInjectVals args)
            && Map.null (requestRetainedGenerations args))
            (throwIO CheckedPurposeMismatch)
      pure scope
    pure (AdmittedRequest exact)
  case admitted of
    Left failure -> reportDiags (Left failure)
    Right request -> case requestDeclarationJoin args of
      Just manifest -> runDeclarationOperation compilerScope args manifest
      Nothing -> dispatchSource compilerScope caches timing request args

dispatchSource :: CompilerScope -> RecoveryCaches -> Bool -> AdmittedRequest -> WorkerRequest -> IO ExitCode
dispatchSource compilerScope caches timing request args =
  let admittedScope = compilerScope
        { scopedCompile = \selection retained purpose session source includes products ->
            scopedCompile compilerScope selection retained
              (maybe purpose (ExactScopeCompile purpose) (admittedRequestScope request))
              session source includes products
        }
      compiler :: Compiler
      compiler = scopedCompile admittedScope
  in
  case requestFiles args of
    [] -> reportDiags (Left (toException (userError "worker request contains no input")))
    (file : _)
        -- Classification consumes every input; all other modes use the first.
        | isJust (requestInspectTypeBatch args)
          && not (length (requestInspections args) > 1 && all isInspectionTypeQuery (requestInspections args))
                                                  -> reportDiags (Left (toException (userError "inspection type batch requires at least two type queries and no other query kinds")))
        | requestActivationPreview args          -> runActivationPreviewMode (scopedParserFlags compilerScope) compiler caches request args file
        | requestCheckSource args                 -> runSourceCheckMode compiler args file
        | requestCellPlan args                    -> runCellPlanMode (scopedParserFlags compilerScope) args file
        | requestCell args                        -> runCellMode admittedScope caches request args file
        | requestClassify args                    -> runClassifyMode (scopedParserFlags compilerScope) timing args
        | not (null (requestInspections args))    -> runInspectionMode admittedScope args file
        -- A turn may also carry session fields, so it precedes session dispatch.
        | requestTurn args                        -> runTurnMode admittedScope caches request args file
        -- Multi-target compilation may also carry a stable-value scope.
        | not (null (requestTargets args))        -> timePhase timing "total" (processFile compiler caches timing args file)
        -- Normal one-shot extraction.
        | otherwise                           -> timePhase timing "total" (processFile compiler caches timing args file)

-- Compile a pure function over an already mounted input. No value interface
-- or authored completion is issued by either the opaque probe or final pass.
runActivationPreviewMode :: DynFlags -> Compiler -> RecoveryCaches -> AdmittedRequest -> WorkerRequest -> FilePath -> IO ExitCode
runActivationPreviewMode parserFlags compiler caches request args path = do
  timing <- readTimingEnabled
  lastAttempt <- newIORef Nothing
  result <- timePhase timing "total" $ trySynchronous $ do
    exact <- requireArg "--session-artifacts" (admittedRequestScope request)
    admission <- maybe (throwIO CheckedPurposeMismatch) pure (scopeActivationPreview exact)
    let readTemplate = withBinaryFile path ReadMode $ \handle -> do
          bytes <- BS.hGet handle ((32 * 1024 * 1024) + 1)
          when (BS.length bytes > 32 * 1024 * 1024) (fail "activation preview template exceeds source bound")
          pure bytes
    originalBytes <- readTemplate
    unless (shaHex originalBytes == previewTemplateSha256 admission)
      (fail "activation preview template differs from its protected offer")
    original <- either (fail . show) (pure . T.unpack) (TE.decodeUtf8' originalBytes)
    let outDir = fromMaybe (takeDirectory path </> takeBaseName path ++ "_cbor") (requestOutDir args)
        opaque = "(TidepoolScaffoldText.pack \"<opaque value>\\nUse the input type to select fields or apply sessionInput.\", False)"
        basePurpose = HostActivationPreviewCompile (previewInputSignature admission)
        render body = do
          source <- replaceRecipeMarker "{{ACTIVATION_PREVIEW}}" body original
          let owner = generatedScaffoldModuleName
                ["activation-preview-scaffold-v1", scopeSemanticSha256 exact
                , previewAdmissionDigest admission, source]
          protected <- either fail pure (renameScaffoldModuleHeader owner original)
          named <- replaceRecipeMarker "{{ACTIVATION_PREVIEW}}" body protected
          (_, moduleName', modulePath) <- writeSplicedModule outDir lastAttempt named
          recipe <- generatedActivationPreviewRecipe parserFlags (previewOriginalInterfaces admission)
            (previewOriginalTarget admission) protected named modulePath moduleName' >>= either fail pure
          pure (named, modulePath, GeneratedScaffoldCompile recipe basePurpose)
    (_, probePath, probePurpose) <- render opaque
    probe <- compiler CheckedEnvironment Set.empty probePurpose (Just (scopeFromWorkerRequest args))
      probePath (requestIncludes args) (requestBuildProductsDir args)
    input <- either fail pure (activationPreviewInputType (crTargetTcGblEnv probe))
    rendered <- satisfiesCapturedConstraint (crHscEnv probe) (crTargetTcGblEnv probe)
      "__tidepoolActivationConstraint" input
    (source, modulePath, purpose) <- render (if rendered
      then "TidepoolInspection.workbenchActivationDisplay " ++ show (previewBudget admission) ++ " __activationInput"
      else opaque)
    prepared <- compiler (PreparedProducts (requestModuleCandidates args)) Set.empty purpose
      (Just (scopeFromWorkerRequest args)) modulePath (requestIncludes args) (requestBuildProductsDir args)
    let compiled = pprPipelineResult prepared
        environment = prHscEnv compiled
        binds = prBinds compiled
    verifiedInput <- either fail pure (activationPreviewInputType (prTargetTcGblEnv compiled))
    originalInterfaces <- newPreparedOriginalInterfaceArtifacts prepared outDir
    witness <- captureCheckedTypeWitness environment verifiedInput
      >>= maybe (fail "activation preview input type has no complete canonical witness") pure
    sealed <- sealCheckedTypeWitness originalInterfaces witness
      >>= maybe (fail "activation preview input lacks an original interface seal") pure
    either fail pure (validateOriginalInputTypeWitness (previewInputSignature admission)
      (previewInputWitness admission) sealed)
    encodedWitness <- maybe (fail "activation preview witness is unsealed") pure (encodeCheckedTypeWitness sealed)
    let witnessBytes = toStrictByteString encodedWitness
    projection <- try (prepareArtifactsWithProjection requirePreviewProjection
      originalInterfaces outDir caches prepared [preparedScaffoldTargetName]
      (standardAuxiliaryRoots binds) Map.empty [])
      :: IO (Either PreviewOriginalDependenciesUnavailable ([PreparedArtifact], Maybe PreparedProductContext))
    validateDependencyEvidence (preparedFreshDependencies prepared)
    revalidateExactScope environment exact >>= either fail pure
    templateBytes <- readTemplate
    unless (templateBytes == originalBytes) (fail "activation preview template changed during compilation")
    let text = encodeString . T.pack
    case projection of
      Left _ -> BS.writeFile (outDir </> "activation-preview-unavailable.cbor") $ toStrictByteString
        (encodeListLen 7 <> text "TPEXACTACTIVATIONPREVIEWUNAVAILABLE1" <> text "1"
          <> text (scopeRequestSha256 exact) <> text (previewAdmissionDigest admission)
          <> encodeWord64 (previewGeneration admission) <> text (previewTemplateSha256 admission)
          <> encodeBytes (previewInputWitness admission))
      Right (artifacts, productContext) -> do
        unless (all (null . paYieldSites) artifacts) (fail "pure activation preview emitted suspension sites")
        writePreparedSidecars InlineYieldSites outDir binds (prTyCons compiled)
          (T.pack <$> prCapturedType compiled) (map T.pack (prWarnings compiled)) artifacts
        writePreparedArtifacts outDir artifacts
        void $ writeRequestProducts args originalInterfaces outDir prepared productContext artifacts
        outFile <- requireArg "--turn-out" (requestTurnOut args)
        BS.writeFile outFile (encodeTurnOut (TExpr 0 (concatMap paYieldSites artifacts) (T.pack source)))
        BS.writeFile (outDir </> "activation-preview.cbor") $ toStrictByteString
          (encodeListLen 8 <> text "TPEXACTACTIVATIONPREVIEW1" <> text "1"
            <> text (scopeRequestSha256 exact) <> text (previewAdmissionDigest admission)
            <> encodeWord64 (previewGeneration admission) <> text (shaHex (TE.encodeUtf8 (T.pack source)))
            <> encodeBytes witnessBytes <> text (if rendered then "rendered" else "opaque"))
  case result of
    Left _ -> readIORef lastAttempt >>= mapM_ (\(output, source) -> do
      void (try (writeFile output source) :: IO (Either IOException ())))
    Right _ -> pure ()
  reportDiags result

runSourceCheckMode :: Compiler -> WorkerRequest -> FilePath -> IO ExitCode
runSourceCheckMode compiler args path = do
  checked <- trySynchronous $ do
    compiled <- compiler (maybe CheckedEnvironment CheckedEnvironmentProducts (requestModuleCandidates args))
      Set.empty GeneralCompile Nothing path (requestIncludes args) (requestBuildProductsDir args)
    pure (crWarnings compiled)
  reportDiagsWithWarnings checked

runDeclarationOperation :: CompilerScope -> WorkerRequest -> FilePath -> IO ExitCode
runDeclarationOperation compilerScope args manifest = do
  result <- trySynchronous $ do
    out <- maybe (fail "declaration operation requires an output path") pure
      (requestDeclarationJoinOut args)
    operation <- readDeclarationOperation manifest
    withScopedExactInterfaceTransaction compilerScope (requestIncludes args) $ \operations ->
      case operation of
        InspectInventory artifacts -> renderDeclarationInventoryOutcome
          <$> inspectDeclarationArtifacts operations artifacts >>= writeFile out
        ValidateJoin input -> renderDeclarationJoinOutcome
          <$> validateDeclarationJoin operations input >>= writeFile out
        EmitHostBindingInterface input -> do
          (binder, issuedPurpose) <- emitHostBindingInterface operations (hostInterfaceProducer input)
            (hostInterfaceGeneration input) (hostInterfaceBinder input) (hostInterfaceSignature input)
            (hostInterfaceScope input) (hostInterfaceRoot input) (hostInterfacePurpose input)
          let generation = hostInterfaceGeneration input
              owner = SessionModule ValMod (Generation generation)
          interface <- BS.readFile (sessionHiPath (hostInterfaceRoot input) owner)
          packages <- BS.readFile (sessionHiPath (hostInterfaceRoot input) owner ++ ".packages")
          requirements <- BS.readFile (sessionHiPath (hostInterfaceRoot input) owner ++ ".requirements")
          BS.writeFile out (toStrictByteString $
            encodeListLen 12 <> encodeString "TPHOSTBINDINGINTERFACERECEIPT" <> encodeString "2"
            <> encodeString (T.pack (shaHex (encodeHostBindingInterface input)))
            <> encodeString (T.pack (hostInterfaceProducer input))
            <> encodeString (T.pack (hostInterfaceAdmission input))
            <> encodeWord64 generation <> encodeBoundBinder binder
            <> encodeString (T.pack (shaHex interface))
            <> encodeString (T.pack (shaHex (toStrictByteString
              (encodeCheckedSignature (hostInterfaceSignature input)))))
            <> encodeString (T.pack (shaHex packages))
            <> encodeString (T.pack (shaHex requirements))
            <> encodeBindingInterfacePurpose issuedPurpose)
  reportDiags result

runInspectionMode :: CompilerScope -> WorkerRequest -> FilePath -> IO ExitCode
runInspectionMode compilerScope args _path = do
  let compiler :: Compiler
      compiler = scopedCompile compilerScope
  res <- trySynchronous $ do
    out <- maybe (fail "inspection request is missing its output path") pure (requestInspectOut args)
    let scope = if hasSessionScope args then Just (scopeFromWorkerRequest args) else Nothing
        compile purpose path = compiler CheckedEnvironment Set.empty purpose scope path
          (requestIncludes args) (requestBuildProductsDir args)
        inspect successful requests = scopedRunGhc compilerScope $ runInspectionGhc
          (crHscEnv successful)
          (crTargetTcGblEnv successful)
          (crTargetRdrEnv successful)
          (crInspectionProbes successful) requests
    results <- runInspectionRequests args compile inspect
    BS.writeFile out (encodeInspectionResults results)
  reportDiags res


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
    originalInterfaces <- newPreparedOriginalInterfaceArtifacts prepared outDir
    (preparedArtifacts, productContext) <- prepareArtifacts originalInterfaces outDir caches prepared preparedTargets
      (standardAuxiliaryRoots binds) (requestRetainedGenerations args) []
    if null preparedArtifacts
      then ioError (userError "prepared extraction requires --target or --targets")
      else timePhase timing "prepared_sidecars" $ writePreparedSidecars SeparateYieldSites outDir binds tycons mCapturedTy warnTexts preparedArtifacts

    timePhase timing "prepared_write" $ writePreparedArtifacts outDir preparedArtifacts
    void $ timePhase timing "module_products" $
      writeRequestProducts args originalInterfaces outDir prepared productContext preparedArtifacts

  reportDiags res

-- Every completed worker product carries its original input evidence. Exact
-- and session requests use their separate retained authority instead.
writeRequestProducts
  :: WorkerRequest -> OriginalInterfaceArtifacts -> FilePath -> PreparedPipelineResult -> Maybe PreparedProductContext
  -> [PreparedArtifact] -> IO CertifiedOriginalProducts
writeRequestProducts args originalInterfaces outDir prepared productContext preparedArtifacts = do
  products <- writeCertifiedProductsKeepingWithOriginals (requestIncludes args) originalInterfaces outDir prepared productContext
    [(paTarget artifact, paProgram artifact) | artifact <- preparedArtifacts]
  when (not (requestCell args) && not (requestActivationPreview args)
      && null (requestInjectVals args) && not (isJust (requestSessionArtifacts args))
      && Map.null (requestRetainedGenerations args)
      && isNothing (preparedExactCompilation prepared)) $
    writeCompileInputProof outDir (prHscEnv (pprPipelineResult prepared))
      (preparedFreshDependencies prepared) (pprPackageImports prepared)
  pure products

data PreparedArtifact = PreparedArtifact
  { paTarget :: String
  , paProgram :: WireProgram
  , paBytes :: BS.ByteString
  , paConstructors :: [DataCon]
  , paYieldSites :: [Tidepool.EffectSchema.YieldSite]
  }

-- Project before writing artifacts so the shared constructor
-- table includes exactly the GHC constructors admitted by prepared execution.
prepareArtifacts :: OriginalInterfaceArtifacts -> FilePath -> RecoveryCaches -> PreparedPipelineResult
  -> [String] -> [String] -> Map.Map SymbolIdentity Word64 -> [HostBindingRepresentation]
  -> IO ([PreparedArtifact], Maybe PreparedProductContext)
prepareArtifacts = prepareArtifactsWithProjection requireProjection

prepareArtifactsWithProjection
  :: (forall a. Either ProjectionError a -> IO a)
  -> OriginalInterfaceArtifacts -> FilePath -> RecoveryCaches -> PreparedPipelineResult
  -> [String] -> [String] -> Map.Map SymbolIdentity Word64 -> [HostBindingRepresentation]
  -> IO ([PreparedArtifact], Maybe PreparedProductContext)
prepareArtifactsWithProjection _ _ _ _ _ [] _ _ _ = pure ([], Nothing)
prepareArtifactsWithProjection project originalInterfaces outDir caches prepared targets@(firstTarget : _) auxiliaryRoots retainedGenerations hostBindings = do
  recoveryCaches <- acquireRecoveryCaches caches
  timing <- readTimingEnabled
  let result = pprPipelineResult prepared
      hscEnv = prHscEnv result
      interfaces = pprProductInterfaces prepared
      modules = pprModules prepared
      candidates = pprAcceptedCandidates prepared
      exactScope = compilationScope <$> preparedExactCompilation prepared
      targetModule = tcg_mod (prTargetTcGblEnv result)
      matching = [preparedModule | preparedModule <- modules,
        pmModule preparedModule == targetModule]
  preparedModule <- case matching of
    [value] -> pure value
    values -> ioError (userError ("prepared target module selection was not unique: " ++ show (length values)))
  firstContext <- prepareCompilerProjectionContext prepared retainedGenerations (pmModule preparedModule)
    firstTarget auxiliaryRoots (case mapMaybe hostBindingRepresentationJsonAuthority hostBindings of
      authority : _ -> Just authority
      [] -> Nothing)
  let contextFor target = firstContext
        { projectionEntry = (projectionEntry firstContext) {symbolOccurrence = T.pack target} }
  let exactProducts = maybe [] scopeAvailableOriginalProducts exactScope
      externalOriginalBinders = Set.fromList
        ([binder | product <- exactProducts, group <- originalGroups product
          , binder <- originalBinders group]
         ++ [binder | candidate <- candidates, group <- candidateGroups candidate
          , binder <- candidateGroupBinders group])
  let prepareOriginal executor = do
        acquired <- recoveryOriginalWorklist caches
        case acquired of
          Just worklist -> prepareOriginalProductsWithWorklist worklist
            hscEnv exactScope interfaces (contextFor firstTarget) externalOriginalBinders modules
          Nothing -> prepareOriginalProductsWithCache
            (compilerPreparedBodies recoveryCaches) (Just (compilerOriginalProjections recoveryCaches)) executor
            hscEnv exactScope interfaces (contextFor firstTarget) externalOriginalBinders modules
  (originalModules,rawProductContext) <- timePhase timing "prepared_original_demand" $
    case recoveryExecutor caches of
      Just executor -> prepareOriginal executor
      Nothing -> withCompilerExecutor serialCompilerExecutionGrant prepareOriginal
  productContext <- admitCurrentOriginalProducts originalInterfaces outDir prepared rawProductContext
  inventory <- maybe (fail "current original admission did not issue its inventory") pure
    (preparedCurrentOriginalInventory productContext)
  originalConstructors <- project (preparedModuleProductConstructors (preparedProductInventory productContext))
  originalSites <- project (preparedModuleProductYieldSites (preparedProductInventory productContext))
  let products = preparedProductInventory productContext
      roots = Set.fromList (map (projectionEntry . contextFor) targets
        ++ projectionAuxiliaryRoots firstContext)
      originalBindings = currentOriginalBindingsExcept inventory roots
      withOriginals context = context {projectionCurrentOriginals = originalBindings}
      admittedOriginalBinders = Set.union externalOriginalBinders (currentOriginalBinders inventory)
  let originalProducts =
        [(unitString (moduleUnit owner), moduleNameString (moduleName owner),
          either (Left . show) Right outcome)
        | (owner, outcome) <- preparedModuleProductOutcomes products]
      originalPackageGlobals = requiredOriginalPackageGlobalsWithRetained
        originalProducts candidates exactOriginals (Map.keysSet retainedGenerations)
      exactOriginals =
        [(originalUnit originalProduct, originalModule originalProduct,
          [(originalOrdinal group, originalBinders group, originalGlobals group)
           | group <- originalGroups originalProduct])
        | originalProduct <- unrecoveredExactProducts (currentReconciledOriginalProducts inventory)]
  finalCandidates <- newIORef Map.empty
  let demandPackages entry packageRoots recovered = do
        let target = T.unpack (symbolOccurrence entry)
            roots = Set.toAscList (Set.fromList (map preparedRootIdentity packageRoots))
            finalContext = (withOriginals (contextFor target))
              { projectionAuxiliaryRoots = projectionAuxiliaryRoots (contextFor target) ++ roots }
        requirePreparedRecoveryPublication target recovered
        selected <- timePhase timing "prepared_project" $
          project (prepareComponentProjectionWithReachability finalContext
            (closureHomeModules recovered) (closureComponentSelections recovered) (closureReachability recovered))
        candidate <- project (projectSelectedCandidateWithHostBindings hostBindings selected)
        project (requireOriginalExecutableGlobals hscEnv admittedOriginalBinders (candidateGlobals candidate))
        required <- either (ioError . userError) pure (originalPackageGlobals (candidateGlobals candidate))
        let additions = Set.toAscList (Set.fromList required `Set.difference` Set.fromList roots)
        if null additions then do
          modifyIORef' finalCandidates (Map.insert entry (candidate,roots))
          pure []
        else forM additions $ \identity -> do
          (identifier,_) <- resolvePackageGlobal hscEnv identity >>= either (ioError . userError) pure
          when (preparedRootIdentity identifier /= identity) $
            ioError (userError "package recovery root differs from canonical original global")
          pure identifier
  recover <- newPreparedRecoveryWithDemand (recoveryExecutor caches) demandPackages
    hscEnv (compilerFatIface recoveryCaches) (compilerOwnerIface recoveryCaches)
    (compilerPreparedBodies recoveryCaches) (withOriginals (contextFor firstTarget)) originalModules []
  artifacts <- forM targets $ \target -> do
    let context = withOriginals (contextFor target)
    -- The same component ledger closes ordinary dependencies and exact
    -- original-package demand before the final candidate is flattened.
    closed <- timePhase timing "prepared_recover" (recover (projectionEntry context))
    let recovered = preparedRecoveryClosure closed
    candidatesByTarget <- readIORef finalCandidates
    (candidate,roots) <- maybe (fail "closed recovery did not issue its final projected candidate") pure
      (Map.lookup (projectionEntry context) candidatesByTarget)
    (program,constructors) <- project (finalizePreparedCandidate candidate)
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
    allSites <- either (fail . ("conflicting executable site metadata: " ++) . show) pure
      (Tidepool.EffectSchema.mergeYieldSites (yieldSites ++ originalSites))
    sealedSites <- forM allSites $ \site -> do
      witnesses <- forM (Tidepool.EffectSchema.ysInputTypeWitnesses site) $ \witness ->
        maybe (pure Nothing) (sealCheckedTypeWitness originalInterfaces) witness
      pure site { Tidepool.EffectSchema.ysInputTypeWitnesses = witnesses }
    pure (PreparedArtifact target program bytes (constructors ++ originalConstructors) sealedSites)
  pure (artifacts, Just productContext)

-- The pure preview boundary preserves only this chosen executable-demand
-- failure. Every other projection, source, authority, or IO failure keeps its
-- ordinary rejection path.
newtype PreviewOriginalDependenciesUnavailable = PreviewOriginalDependenciesUnavailable [SymbolIdentity]
  deriving Show
instance Exception PreviewOriginalDependenciesUnavailable

requirePreviewProjection :: Either ProjectionError a -> IO a
requirePreviewProjection = \case
  Left (UnavailableOriginalHomeDependencies identities) ->
    throwIO (PreviewOriginalDependenciesUnavailable identities)
  result -> requireProjection result

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

data YieldSiteDelivery = SeparateYieldSites | InlineYieldSites

writePreparedSidecars
  :: YieldSiteDelivery -> FilePath -> [CoreBind] -> [TyCon] -> Maybe T.Text -> [T.Text]
  -> [PreparedArtifact] -> IO ()
writePreparedSidecars delivery outDir binds tycons capturedType warnings artifacts = do
  let constructors = concatMap paConstructors artifacts
  metadata <- either (ioError . userError . show) pure (metadataForConstructors tycons constructors)
  let hasIO = any (targetBindingHasIO binds . paTarget) artifacts
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
runTurnMode :: CompilerScope -> RecoveryCaches -> AdmittedRequest -> WorkerRequest -> FilePath -> IO ExitCode
runTurnMode compilerScope caches request args path = do
  let compiler :: Compiler
      compiler = scopedCompile compilerScope
  timing <- readTimingEnabled
  hPutStrLn stderr $ "Processing (turn): " ++ path
  lastAttempt <- newIORef Nothing
  res <- timePhase timing "total" $ trySynchronous $ do
    turnSrc   <- readFile path
    let templates = requestTurnTemplates args
    mVerdict  <- traverse parseTurnVerdictArg (requestTurnVerdict args)
    -- The parse emits no phases of its own. This mode times it as the single
    -- @classify@ phase, emitted
    -- only on the branch that actually classifies. With @--turn-verdict@
    -- supplied nothing is parsed, and an absent @classify@ row is the
    -- honest report rather than a phantom 0ms line.
    let parserFlags = scopedParserFlags compilerScope
    sb <- case mVerdict of
      Just verdict -> pure verdict
      Nothing -> timePhase timing "classify"
        (evaluate (classifyWithFlags parserFlags turnSrc))
    let exact = admittedRequestScope request
    let admittedItem = exact >>= scopeCheckedItem
    forM_ admittedItem $ \admission -> validateCheckedItemAdmission args admission turnSrc sb
    let outDir     = fromMaybe (takeDirectory path </> takeBaseName path ++ "_cbor") (requestOutDir args)
        bindersStr = intercalate ", " (sbBinders sb)
    turnOut <- case sbKind sb of
      KDecl -> do
        tmplFile <- case lookup (templateSelectorWireName SDecl) templates of
          Just f  -> return f
          Nothing -> error "--turn: no --turn-template for kind decl"
        tmplSrc <- readFile tmplFile
        declarationSource <- declarationSourceWithTemplateFlags parserFlags tmplSrc turnSrc
          >>= either throwCellSplitError pure
        spliced <- either fail pure (renderDeclarationForTemplate tmplSrc declarationSource)
        (_spliced, modName, modulePath) <- writeSplicedModule outDir lastAttempt spliced
        items <- timePhase timing "declaration_binders" $ scopedRunGhc compilerScope (extractBindersNamedGhc modulePath (requestIncludes args) modName)
        let binders = if null (sbBinders sb)
                        then map (T.pack . exportItemName) items
                        else map T.pack (sbBinders sb)
        return (TDecl binders items declarationSource)
      _kind -> do
        compileClassifiedTurn parserFlags compiler caches args timing outDir turnSrc sb bindersStr admittedItem lastAttempt exact
    outFile <- requireArg "--turn-out" (requestTurnOut args)
    let cbor = encodeTurnOut turnOut
    BS.writeFile outFile cbor
    forM_ (exact >>= \scope -> (,) scope <$> scopeCheckedItem scope) $ \(scope,admission) ->
      case turnOut of
        TBind _ _ _ _ wrapped -> writeCheckedItemReceipt outDir scope admission (T.unpack wrapped)
        TExpr _ _ wrapped -> writeCheckedItemReceipt outDir scope admission (T.unpack wrapped)
        TDecl {} -> fail "checked recipe cannot compile an unproved declaration"
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

-- | Compile one non-declaration turn from authored syntax or an admitted
-- native recipe. Checked signatures remain compiler payloads, never type text.
compileClassifiedTurn
  :: DynFlags -> Compiler -> RecoveryCaches -> WorkerRequest -> Bool -> FilePath
  -> String -> StmtBinders -> String -> Maybe CheckedItemAdmission
  -> IORef (Maybe (FilePath, String))
  -> Maybe ExactScope
  -> IO TurnOut
compileClassifiedTurn flags compiler caches args timing outDir turnSrc sb bindersStr admitted lastAttempt exact =
  compiledTurn <$> compileClassifiedTurnKeeping compiler caches args timing outDir turnSrc sb bindersStr admitted lastAttempt [] (StandaloneTurnParse flags) exact

data CompiledTurnOutput = CompiledTurnOutput
  { compiledTurn :: TurnOut
  , compiledPipeline :: PreparedPipelineResult
  , compiledOriginalProducts :: CertifiedOriginalProducts
  , compiledModule :: String
  }

data TurnParseContext
  = StandaloneTurnParse DynFlags
  | PlannedTurnParse DynFlags SourcePrologue

compileClassifiedTurnKeeping
  :: Compiler -> RecoveryCaches -> WorkerRequest -> Bool -> FilePath
  -> String -> StmtBinders -> String -> Maybe CheckedItemAdmission
  -> IORef (Maybe (FilePath, String)) -> [String] -> TurnParseContext -> Maybe ExactScope -> IO CompiledTurnOutput
compileClassifiedTurnKeeping compiler caches args timing outDir turnSrc sb bindersStr admitted lastAttempt programImports parseContext exact = do
    let (parserFlags, prologue) = case parseContext of
          StandaloneTurnParse flags -> (flags, Nothing)
          PlannedTurnParse flags authored -> (flags, Just authored)
        semanticScopeFields = case exact of
          Just selected -> ["exact-scope-semantic", scopeSemanticSha256 selected]
          Nothing ->
            let scope = scopeFromWorkerRequest args
            in (["session-scope-root", ssRoot scope]
              ++ maybe ["session-incarnation-absent"] (\value -> ["session-incarnation", value]) (ssIncarnation scope)
              ++ concatMap (\value -> ["session-value-interface", sessionModuleString value]) (ssValIfaces scope))
    let templates = requestTurnTemplates args
        protectedTemplates = itemTurnTemplates <$> admitted
        templateImports = maybe (CheckedTemplateImports [] []) itemTemplateImports admitted
        verifyProtectedTemplate path captured = forM_ protectedTemplates $ \expected ->
          unless (any (\(kind,file) -> file == path
              && lookup kind expected == Just (shaHex (TE.encodeUtf8 (T.pack captured)))) templates)
            (fail "generated scaffold template differs from its protected offer")
        basePurpose = maybe id withSourceImportIntents prologue $
          maybe GeneralCompile checkedItemCompilePurpose admitted
        -- Splice @tmplFile@ against the turn text and write the generated
        -- module under the name in its compiler-owned header. Non-declaration
        -- turns derive a stable owner from their semantic compile inputs;
        -- authored declaration templates retain their declared module name.
        -- The scratch basename must match the header because
        -- 'runPipelineSessionSelected' resolves the compiled module by
        -- @capitalize (takeBaseName path)@ (GhcPipeline.hs).
        spliceInto :: FilePath -> IO (String, String, RenderedProtectedTemplateImports, String, FilePath)
        spliceInto tmplFile = do
          originalTemplate <- readFile tmplFile
          verifyProtectedTemplate tmplFile originalTemplate
          selectedFlags <- templateParserFlags parserFlags originalTemplate >>= either throwCellSplitError pure
          defaults <- either fail pure (captureCompilerDefaultRecipe selectedFlags originalTemplate)
          let original = admitted >>= itemPlannedDeclaration
          let prepareTemplate template = do
                let pragmaPrefix = maybe "" (concatMap ((++ "\n") . locatedPragmaSource) . prologuePragmas) prologue
                tmplSrc <- case prologue of
                  Nothing -> pure template
                  Just authored -> do
                    replaceRecipeMarker preambleImportMarker
                      (concatMap ((++ "\n") . locatedImportSource) (prologueImports authored)
                        ++ preambleImportMarker) template
                withOriginal <- case original of
                  Nothing -> pure tmplSrc
                  Just ((_, owner), _) -> replaceRecipeMarker preambleImportMarker
                    ("import " ++ owner ++ "\n" ++ preambleImportMarker) tmplSrc
                withProgram <- if null programImports then pure withOriginal else
                  replaceRecipeMarker preambleImportMarker
                    (concatMap (\owner -> "import " ++ owner ++ "\n") programImports ++ preambleImportMarker) withOriginal
                pure (pragmaPrefix ++ withProgram, length (filter (== '\n') pragmaPrefix))
              namespaces = maybe [] (concatMap locatedImportNamespaces . prologueImports) prologue
                ++ map mkModuleName programImports
                ++ maybe [] (\((_,owner),_) -> [mkModuleName owner]) original
                ++ maybe [] (map (mkModuleName . fst) . itemValueImports) admitted
              renderRecipe (withProgram, protectedLinePrefix) = do
                case admitted of
                  Nothing -> do
                    (prepared, qualifiedLinePrefix) <- either fail pure
                      (qualifyCompilerDefaultWithLineOffset defaults namespaces withProgram)
                    pure (spliceTemplate prepared turnSrc bindersStr, protectedLinePrefix + qualifiedLinePrefix)
                  Just admission -> do
                    withPrefix <- if null (itemValueImports admission) then pure withProgram else
                      replaceRecipeMarker preambleImportMarker
                        (concatMap (\(moduleName',names) -> "import " ++ moduleName' ++ " (" ++ intercalate ", " (map renderImportBinder names) ++ ")\n")
                          (itemValueImports admission) ++ preambleImportMarker) withProgram
                    (source, qualifiedLinePrefix) <- checkedRecipeSourceWithLineOffset
                      defaults namespaces admission withPrefix turnSrc
                    pure (source, protectedLinePrefix + qualifiedLinePrefix)
              authorityFields = case admitted of
                Just authority -> ["item", itemAdmissionDigest authority, itemCellReceiptDigest authority]
                Nothing -> ["unadmitted"]
              includeFields = case admitted of
                Nothing -> ["source-search-includes"]
                  ++ concatMap (\include -> ["include-path", include]) (requestIncludes args)
                _ -> ["checked-import-authority"]
              retainedFields = concat
                [ ["retained", T.unpack (symbolUnit identity), T.unpack (symbolModule identity)
                  , T.unpack (symbolNamespace identity), T.unpack (symbolOccurrence identity)]
                  ++ maybe ["no-record-parent"] (\parent -> ["record-parent", T.unpack parent])
                    (symbolRecordParent identity)
                  ++ [show generation]
                | (identity, generation) <- Map.toAscList (requestRetainedGenerations args) ]
              renderNamedRecipe = do
                withProgram <- prepareTemplate originalTemplate
                (baseSource, baseLineOffset) <- renderRecipe withProgram
                if sbKind sb == KDecl
                  then do
                    protectedOccurrences <- either fail pure
                      (captureProtectedTemplateImports selectedFlags originalTemplate)
                    renderedOccurrences <- either fail pure
                      (renderProtectedTemplateImports baseLineOffset protectedOccurrences)
                    pure (originalTemplate, baseSource, renderedOccurrences)
                  else do
                    let owner = generatedScaffoldModuleName
                          (["turn-scaffold-owner-v1", "scope"] ++ semanticScopeFields
                            ++ ["template", originalTemplate, "rendered", baseSource
                              ] ++ includeFields ++ authorityFields ++ retainedFields)
                    renamed <- either fail pure (renameScaffoldModuleHeader owner originalTemplate)
                    protectedOccurrences <- either fail pure
                      (captureProtectedTemplateImports selectedFlags renamed)
                    renamedTemplate <- prepareTemplate renamed
                    (finalSource, linePrefix) <- renderRecipe renamedTemplate
                    renderedOccurrences <- either fail pure
                      (renderProtectedTemplateImports linePrefix protectedOccurrences)
                    pure (renamed, finalSource, renderedOccurrences)
          (finalProtected, finalSource, finalOccurrences) <- renderNamedRecipe
          finalOutput <- writeSplicedModule outDir lastAttempt finalSource
          let (_,moduleName',modulePath) = finalOutput
          pure (finalProtected, finalSource, finalOccurrences, moduleName',modulePath)
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
        compileTurn protected spliced protectedOccurrences modName modulePath = do
          purpose <- case protectedTemplates of
            Nothing -> pure basePurpose
            Just _ -> do
              recipe <- generatedScaffoldRecipeWithProtectedImports parserFlags templateImports
                protectedOccurrences protected spliced modulePath modName >>= either fail pure
              pure (GeneratedScaffoldCompile recipe basePurpose)
          compiler (PreparedProducts (requestModuleCandidates args))
            (Map.keysSet (requestRetainedGenerations args)) purpose
            (Just scope) modulePath (requestIncludes args)
            (requestBuildProductsDir args)
        compileVariants _ [] = error ("--turn: no --turn-template for kind " ++ templateSelectorWireName selector)
        compileVariants _ ((index,tmplFile):rest) = do
          (protected, spliced, protectedOccurrences, modName, modulePath) <- spliceInto tmplFile
          attempted <- try (compileTurn protected spliced protectedOccurrences modName modulePath)
          case attempted of
            Right prepared ->
              return (index, spliced, prepared)
            Left err@(_ :: SomeException) -> case (sourceFailureDiagnostics err, rest) of
              (Just _, _ : _) | not (isJust admitted) -> compileVariants (index + 1) rest
              _               -> throwIO err
    (variant, spliced, prepared) <- compileVariants (0 :: Int) matching
    let result = pprPipelineResult prepared
        binds       = prBinds result
        hscEnv      = prHscEnv result
        mCapturedTy = fmap T.pack (prCapturedType result)
        warnTexts   = map T.pack (prWarnings result)
    -- Projection remains outside compileVariants. Its entry is the settled
    -- scaffold, and its constructors join the shared metadata before write.
    let boundNames = case selector of
          SBind -> sbBinders sb
          SExpr -> maybe [] pure (admitted >>= itemObservationName)
          _ -> []
    sessionBindings <- prepareSessionBindings boundNames result
    originalInterfaces <- newPreparedOriginalInterfaceArtifacts prepared outDir
    (preparedArtifacts, productContext) <- prepareArtifacts originalInterfaces outDir caches prepared
      [preparedScaffoldTargetName] (standardAuxiliaryRoots binds)
      (requestRetainedGenerations args) (sessionBindingRepresentations sessionBindings)
    let asksSites = concatMap paYieldSites preparedArtifacts
    timePhase timing "prepared_sidecars" $ writePreparedSidecars InlineYieldSites outDir binds (prTyCons result) mCapturedTy warnTexts preparedArtifacts
    timePhase timing "prepared_write" $ writePreparedArtifacts outDir preparedArtifacts
    originalProducts <- timePhase timing "module_products" $
      writeRequestProducts args originalInterfaces outDir prepared productContext preparedArtifacts
    -- Mutable turns never enter the artifact cache, but publication must
    -- still reject source changes observed during this compilation.
    validateDependencyEvidence (preparedFreshDependencies prepared)
    let wrapped = T.pack spliced
    turn <- case selector of
      SBind -> do
        g    <- requireArg "--bind-gen"     (requestBindGen args)
        root <- requireArg "--session-root" (requestSessionRoot args)
        bbs  <- writeSessionBindings g root sessionBindings
        return (TBind (map T.pack (sbBinders sb)) variant bbs asksSites wrapped)
      SBindDiscard -> return (TBind [] variant [] asksSites wrapped)
      SExpr -> case admitted >>= itemObservationName of
        Just observation -> do
          generation <- requireArg "--bind-gen" (requestBindGen args)
          root <- requireArg "--session-root" (requestSessionRoot args)
          bound <- writeSessionBindings generation root sessionBindings
          return (TBind [T.pack observation] variant bound asksSites wrapped)
        Nothing -> return (TExpr variant asksSites wrapped)
      SDecl -> error ("--turn: unexpected verdict kind: " ++ templateSelectorWireName selector)
    owner <- maybe (fail "compiled turn has no module owner") pure (extractModuleName spliced)
    pure (CompiledTurnOutput turn prepared originalProducts owner)

-- | Block classify mode (@--classify@):
-- Classify every input using the capability's retained parser defaults and
-- write the verdicts in argv order to
-- @--classify-out@. Serves @tidepool-repl@'s block runner, which segments a
-- block into decl runs before compiling any item and so needs every verdict
-- up front — one spawn for the whole block instead of one classify spawn per
-- item.
runClassifyMode :: DynFlags -> Bool -> WorkerRequest -> IO ExitCode
runClassifyMode parserFlags timing args =
  timePhase timing "total" $
    try
      ( do
          out      <- requireArg "--classify-out" (requestClassifyOut args)
          srcs     <- mapM readFile (requestFiles args)
          verdicts <- timePhase timing "classify" (mapM (evaluate . classifyWithFlags parserFlags) srcs)
          writeFile out (renderVerdictsJson verdicts)
          hPutStrLn stderr $ "  Wrote: " ++ out ++ " (" ++ show (length verdicts) ++ " verdicts)"
      )
      >>= reportDiags

-- | Split, classify, and typecheck one notebook cell in a single worker
-- request. Rust authors the module template (scope/import/effect-row policy);
-- GHC owns every Haskell decision and returns post-zonk statement binder pins.
-- Parsing chooses execution ordinals before runtime reserves original owners.
-- The emitted receipt contains no checked pins, prepared bodies or live values.
runCellPlanMode :: DynFlags -> WorkerRequest -> FilePath -> IO ExitCode
runCellPlanMode parserFlags args cellPath = do
  result <- trySynchronous $ do
    source <- readFile cellPath
    templatePath <- requireArg "--cell-template" (requestCellTemplate args)
    template <- readFile templatePath
    templates <- forM (requestTurnTemplates args) $ \(kind,path) -> do
      bytes <- BS.readFile path
      pure (kind, shaHex bytes)
    plan <- analyzeOrderedCellWithFlags parserFlags template source >>= either throwCellSplitError pure
    let text = encodeString . T.pack
        observation = encodeCellOut plan [] [] ""
        receipt = encodeListLen 8 <> text "TPCELLPLAN3"
          <> text (shaHex (TE.encodeUtf8 (T.pack source)))
          <> text (shaHex (TE.encodeUtf8 (T.pack template)))
          <> encodeListLen (fromIntegral (length templates))
          <> foldMap (\(kind,digest) -> encodeListLen 2 <> text kind <> text digest) templates
          <> encodeListLen (fromIntegral (length (requestIncludes args)))
          <> foldMap text (requestIncludes args)
          <> encodeListLen (fromIntegral (length (requestInjectVals args)))
          <> foldMap text (requestInjectVals args)
          <> encodeBytes observation
          <> encodeListLen (fromIntegral (length (cellPlanItems plan)))
          <> foldMap (text . bindingForm) (cellPlanItems plan)
        bindingForm item = case cellAnalysisBindingForm item of
          Just ActionBinding -> "action"
          Just LetBinding -> "let"
          Just RecursiveBinding -> "recursive"
          Nothing -> "none"
    out <- requireArg "--cell-out" (requestCellOut args)
    BS.writeFile out (toStrictByteString receipt)
  reportDiags result

runCellMode :: CompilerScope -> RecoveryCaches -> AdmittedRequest -> WorkerRequest -> FilePath -> IO ExitCode
runCellMode compilerScope caches request args cellPath = do
  let exact = admittedRequestScope request
  case exact >>= \scope -> (,) scope <$> (scopeCheckedCell scope >>= checkedPlannedCell) of
    Just (scope, planned) -> runCellProgramMode (scopedParserFlags compilerScope) (scopedCompile compilerScope) caches args cellPath scope planned
    Nothing -> runLegacyCellMode (scopedParserFlags compilerScope) (scopedCompile compilerScope) caches request args cellPath

runLegacyCellMode :: DynFlags -> Compiler -> RecoveryCaches -> AdmittedRequest -> WorkerRequest -> FilePath -> IO ExitCode
runLegacyCellMode parserFlags compiler caches request args cellPath = do
  provisionalOutput <- newIORef Nothing
  res <- trySynchronous $ do
    cellSource <- readFile cellPath
    templatePath <- requireArg "--cell-template" (requestCellTemplate args)
    template <- readFile templatePath
    let admittedScope = admittedRequestScope request
    forM_ admittedScope $ \scope -> forM_ (scopeCheckedCell scope) $ \admission ->
      validateCheckedCellAdmission args admission cellSource template
    checkingTemplate <- case admittedScope >>= scopeCheckedCell of
      Nothing -> pure template
      Just _ -> either (throwIO . InvalidCheckingWrapper) pure
        (replaceTemplateModuleHeader "module CellCheck where" template)
    protectedCheckImports <- either fail pure
      (captureProtectedTemplateImportsAt "{{CELL_IMPORTS}}" parserFlags checkingTemplate)
    initialPlan <- analyzeCellWithFlags parserFlags template cellSource >>= either throwCellSplitError pure
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
    let checkPurpose = maybe GeneralCompile (\(_,_,inventory,exact,_) -> PlannedDeclarationCheck inventory exact) preparedDeclaration
        checkedSelection = if isJust preparedDeclaration then CheckedEnvironment else
          maybe CheckedEnvironment CheckedEnvironmentProducts (requestModuleCandidates args)
        checkPlan plan = maybe plan (\(_,planned,_,_,_) -> plannedCheckPlan planned) preparedDeclaration
    (analyzed, provisional) <- checkCellInstances (\plan -> do
      let effective = checkPlan plan
      (rendered, linePrefix) <- either fail pure
        (renderCellCheckSourceWithLineOffset checkingTemplate effective)
      writeFile modulePath rendered
      -- Preserve the latest plan for failure diagnostics without encoding and
      -- writing a provisional result before every successful check attempt.
      writeIORef provisionalOutput (Just (out, plan, rendered))
      let baseCheckPurpose = GeneratedInstanceCheck (cellGeneratedInstanceRecipe effective) checkPurpose
      compilePurpose <- case admittedScope >>= scopeCheckedCell of
        Nothing -> pure baseCheckPurpose
        Just admission -> do
          mappedImports <- either fail pure (renderProtectedTemplateImports linePrefix protectedCheckImports)
          recipe <- generatedCheckingTemplateRecipeWithProtectedImports parserFlags
            (checkedTemplateImports admission) mappedImports checkingTemplate rendered modulePath moduleName'
            >>= either fail pure
          pure (GeneratedScaffoldCompile recipe baseCheckPurpose)
      compiler checkedSelection Set.empty
        (withSourceImportIntents (cellPlanPrologue effective) compilePurpose)
        scope modulePath (requestIncludes args) (requestBuildProductsDir args))
        (maybe initialPlan (\(plan,_,_,_,_) -> plan) preparedDeclaration)
    checkedSource <- either fail pure (renderCellCheckSource checkingTemplate (checkPlan analyzed))
    let (finalPlan, finalSource, compiled) = (analyzed, checkedSource, provisional)
    -- Presentation observations remain separate from native signature authority.
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
            <> text "TPEXACTCHECK" <> text "3" <> text (scopeRequestSha256 receiptScope)
            <> text (checkedAdmissionDigest admission) <> text (checkedCellSha256 admission)
            <> text (checkedTemplateSha256 admission) <> text (shaHex outputBytes)
            <> text (shaHex (TE.encodeUtf8 (T.pack finalSource)))
            <> encodeListLen (fromIntegral (length signatures))
            <> foldMap encodeCheckedSignature signatures
            <> maybe encodeNull (text . shaHex) plannedReceipt
      BS.writeFile (outDir </> "checked-cell.cbor") (toStrictByteString receipt)
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
runCellProgramMode :: DynFlags -> Compiler -> RecoveryCaches -> WorkerRequest -> FilePath
  -> ExactScope -> PlannedCellAdmission -> IO ExitCode
runCellProgramMode parserFlags compiler caches args cellPath exact planned = do
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
    initial <- analyzeOrderedCellWithFlags parserFlags template source >>= either throwCellSplitError pure
    unless (length (cellPlanItems initial) == length (plannedSlots planned))
      (fail "compiled cell reservation count differs from parser")
    root <- requireArg "--session-root" (requestSessionRoot args)
    let outDir = fromMaybe (takeDirectory cellPath </> "cell-program") (requestOutDir args)
        initialState = initialProgramCellState (cellPlanPrologue initial) exact (requestRetainedGenerations args)
    createDirectoryIfMissing True outDir
    batches <- newIORef []
    settled <- foldM (compileSegment parserFlags timing admission template outDir batches)
      initialState (zip [0::Int ..] (cellInferenceSegments initial))
    let finalPlan = initial { cellPlanItems = concatMap cellPlanItems (programPlans settled) }
        checkedSource = concat (programSources settled)
        observations = encodeCellOut finalPlan [] (programObservations settled) checkedSource
        text = encodeString . T.pack
        receipt = encodeListLen 12 <> text "TPEXACTPROGRAM" <> text "4"
          <> text (scopeRequestSha256 exact) <> text (checkedAdmissionDigest admission)
          <> text (checkedCellSha256 admission) <> text (checkedTemplateSha256 admission)
          <> text (shaHex observations) <> text (shaHex (TE.encodeUtf8 (T.pack checkedSource)))
          <> encodeListLen (fromIntegral (length (programCaptureSignatures settled)))
          <> foldMap encodeCheckedSignature (programCaptureSignatures settled)
          <> encodeListLen (fromIntegral (length (programDeclarations settled)))
          <> foldMap (\(index,digest) -> encodeListLen 2 <> encodeWord64 (fromIntegral index) <> text digest)
            (programDeclarations settled)
          <> text (plannedParserDigest planned)
          <> encodeListLen (fromIntegral (length (programTypedPlans settled)))
          <> foldMap encodeTypedSegmentPlan (programTypedPlans settled)
    out <- requireArg "--cell-out" (requestCellOut args)
    sourceNow <- readFile cellPath
    templateNow <- readFile templatePath
    validateCheckedCellAdmission args admission sourceNow templateNow
    unless (sourceNow == source && templateNow == template) (fail "compiled cell source changed")
    completedBatches <- readIORef batches
    foldr (\batch next -> withTypedSegmentSessionPublication root batch next)
      (BS.writeFile out observations >> BS.writeFile (outDir </> "checked-cell.cbor") (toStrictByteString receipt))
      completedBatches
    pure []
  reportDiagsWithWarnings attempted
  where
    compileSegment parserFlags timing admission template outDir batches state (segmentIndex, segment) = do
      let offset = programItemOffset state
          prefix = selectedProgramValues (programValues state)
          withPrefix = installProgramImports prefix (programOriginals state) segment
          scope = programExact state
          localArgs = args { requestInjectVals = map exactModule (maybe [] checkedValueInterfaces (scopeCheckedCell scope))
            , requestRetainedGenerations = programRetained state }
          scoped :: Compiler
          scoped selection retained purpose session path includes products =
            compiler selection retained (ExactScopeCompile (programPurpose state purpose) scope) session path includes products
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
          (finalized,_,inventory,extended,(prepared,certified)) <- timePhase timing "cell_program_declaration" $
            prepareOriginalCellDeclaration scoped caches localArgs template directory
              (Just (scopeFromWorkerRequest localArgs)) scope ownAdmission withPrefix
          retainedImports <- retainProgramSourceImports (programSourceImports state) prepared
            (certifiedFinalizedArtifacts certified) extended
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
          pure $ recordDeclarationSegment finalized (plannedSourceFromDirectory finalized) (shaHex receipt)
            $ state { programExact = extended, programOriginal = original
              , programSourceImports = retainedImports
              , programValues = remainingValues
              , programOriginals = programOriginals state ++ maybe [] pure original }
        _ -> do
          let checkingTemplate = either error id (replaceTemplateModuleHeader "module CellCheck where" template)
              slots = [case plannedSlots planned !! index of
                  PlannedBind generation -> (index,generation,Nothing)
                  PlannedExpression generation observation -> (index,generation,Just observation)
                  _ -> error "executable segment has declaration reservation"
                | index <- [offset .. offset + length (cellPlanItems withPrefix) - 1]]
          preparedBatch <- newIORef Nothing
          preparationAttempt <- newIORef (0 :: Int)
          let compile plan = do
                attempt <- readIORef preparationAttempt
                modifyIORef' preparationAttempt (+ 1)
                writeIORef preparedBatch Nothing
                let stagingRoot = directory </> "typed-session-" ++ show attempt
                base <- either fail pure (prepareTypedSegmentSource checkingTemplate plan
                  (plannedReservationDigest planned) slots)
                let owner = generatedScaffoldModuleName ["typed-segment-owner-v1",
                      scopeRequestSha256 scope, checkedAdmissionDigest admission,
                      preparedTypedSegmentSource base]
                named <- either fail pure (renameScaffoldModuleHeader owner checkingTemplate)
                authored <- either fail pure (prepareTypedSegmentSource named plan
                  (plannedReservationDigest planned) slots)
                unless (preparedTypedSegmentPlan base == preparedTypedSegmentPlan authored)
                  (fail "typed segment owner changed its normalized plan")
                let protected = preparedTypedSegmentTemplate authored
                    rendered = preparedTypedSegmentSource authored
                    typedPlan = preparedTypedSegmentPlan authored
                    sourcePath = directory </> owner ++ ".hs"
                protectedImports <- either fail pure
                  (captureProtectedTemplateImportsAt "{{CELL_IMPORTS}}" parserFlags protected)
                mapped <- either fail pure (renderProtectedTemplateImports
                  (preparedTypedSegmentLineOffset authored) protectedImports)
                writeFile sourcePath rendered
                recipe <- generatedTypedSegmentRecipeWithProtectedImports (preparedTypedSegmentOperations authored) parserFlags
                  (checkedTemplateImports admission) mapped protected rendered sourcePath owner >>= either fail pure
                let prepare environment admissions typed = do
                      batch <- prepareTypedSegmentSessionBindings environment admissions typed stagingRoot
                      writeIORef preparedBatch (Just (stagingRoot,batch))
                      pure (typedSegmentSessionEnvironment batch, typedSegmentSessionGlobals batch,
                        typedSegmentSessionInterfaces batch)
                    purpose = GeneratedScaffoldCompile recipe
                      (GeneratedInstanceCheck (cellGeneratedInstanceRecipe plan)
                        (TypedSegmentCompile typedPlan (preparedTypedSegmentOperations authored) (CheckedItemCompile [] (programOriginal state) prefix)))
                compiledSegment <- scoped (WithTypedSegmentPreparation prepare
                  (PreparedSegmentProducts typedPlan (requestModuleCandidates localArgs)))
                  (Map.keysSet (requestRetainedGenerations localArgs))
                  (withSourceImportIntents (cellPlanPrologue plan) purpose)
                  (Just (scopeFromWorkerRequest localArgs)) sourcePath (requestIncludes args) (requestBuildProductsDir args)
                pure (authored,compiledSegment)
          (finalized,(authored,compiled)) <- timePhase timing "cell_program_segment_frontend"
            (checkCellInstances compile withPrefix)
          (stagingRoot,batch) <- readIORef preparedBatch
            >>= maybe (fail "typed segment has no prepared Session batch") pure
          let prepared = preparedSegmentProducts compiled
              typed = preparedSegmentCaptures compiled
              typedPlan = preparedTypedSegmentPlan authored
              rendered = preparedTypedSegmentSource authored
              result = pprPipelineResult prepared
              items = typedSegmentItems typed
              targets = map (plannedItemEntry . typedItemPlan) items
              binds = prBinds result
              sourceOwner = moduleNameString (moduleName (tcg_mod (prTargetTcGblEnv result)))
          attempts <- readIORef preparationAttempt
          emitCount timing "typed_segment_kept_entries" (toInteger (length items))
          emitCount timing "typed_segment_generated_instance_retries" (toInteger (attempts - 1))
          captureSignatures <- concat <$> forM items (\item ->
            forM (typedItemCaptures item) (\capture -> captureCheckedSignature (prHscEnv result)
              (plannedItemEntry (typedItemPlan item) ++ ":"
                ++ occNameString (nameOccName (idName (typedCaptureIdentifier capture))))
              (typedCaptureType capture)))
          let expressionObservations =
                [CellExpressionPlan (plannedItemEntry (typedItemPlan item))
                  (case typedObservationLift observation of
                    PureObservation -> ExpressionPure
                    EffectfulObservation -> ExpressionEffectful)
                  (showSDoc (hsc_dflags (prHscEnv result)) (ppr (typedObservationValueType observation)))
                  (nominalHeadsOfType (typedObservationValueType observation))
                | item <- items, Just observation <- [typedItemObservation item]]
          originalInterfaces <- newPreparedOriginalInterfaceArtifacts prepared directory
          let producedGenerations = Map.fromList
                [(preparedRootIdentity global,generation)
                | (global,generation) <- typedSegmentSessionRetainedGlobals batch]
          unless (Map.null (Map.intersection producedGenerations (programRetained state)))
            (fail "typed segment output collides with a retained generation")
          let projectionGenerations = Map.union producedGenerations (programRetained state)
          (artifacts,productContext) <- timePhase timing "cell_program_segment_project" $
            prepareArtifacts originalInterfaces directory caches prepared targets (standardAuxiliaryRoots binds)
              projectionGenerations (typedSegmentSessionBindingRepresentations batch)
          unless (length artifacts == length items) (fail "typed segment projection lost an entry")
          currentProducts <- maybe (fail "typed segment has no admitted original inventory") pure productContext
          sharedProducts <- writeCertifiedSegmentProducts (requestIncludes localArgs) originalInterfaces
            directory prepared currentProducts
          emitted <- forM (zip3 items artifacts (cellPlanItems finalized)) $ \(item,artifact,sourceItem) -> do
            let itemPlan = typedItemPlan item
                index = plannedItemOrdinal itemPlan
                generation = plannedItemGeneration itemPlan
                itemDirectory = outDir </> "item-" ++ show index
                captures = case lookup index (typedSegmentSessionBinders batch) of
                  Just values -> values
                  Nothing -> error "typed Session batch lost an item"
                outputArtifact = artifact {paTarget=preparedScaffoldTargetName}
                asks = paYieldSites artifact
                boundNames = map (T.pack . bbName) captures
                turn = TBind boundNames 0 captures asks (T.pack rendered)
                source = cellAnalysisSource sourceItem
                verdict = cellAnalysisVerdict sourceItem
                observation = case plannedItemBody itemPlan of ObservationItem _ name -> Just name; _ -> Nothing
                itemAdmission = CheckedItemAdmission AuthoredCheckedItem (checkedAdmissionDigest admission) (checkedAdmissionDigest admission)
                  (fromIntegral index) (shaHex (TE.encodeUtf8 (T.pack source)))
                  (if sbKind verdict == KBind then "bind" else "expr") (sbBinders verdict)
                  (checkedTurnTemplates admission) (map exactModule (scopeValues scope)) [] Nothing
                  generation (checkedAdmissionDigest admission) (map programValueImport prefix) observation (programOriginal state)
                  prefix (scopeValues scope) (checkedTemplateImports admission)
            createDirectoryIfMissing True itemDirectory
            writeFile (itemDirectory </> sourceOwner ++ ".hs") rendered
            writePreparedSidecars InlineYieldSites itemDirectory binds (prTyCons result)
              Nothing (map T.pack (prWarnings result)) [outputArtifact]
            writePreparedArtifacts itemDirectory [outputArtifact]
            writeCertifiedSegmentItemProducts prepared sharedProducts itemDirectory (paProgram outputArtifact)
            ordinal <- typedEntryOriginalOrdinal sharedProducts (preparedRootIdentity (typedItemRoot item))
            BS.writeFile (itemDirectory </> "turn.cbor") (encodeTurnOut turn)
            writeTypedItemReceipt itemDirectory scope itemAdmission rendered typedPlan
              (preparedRootIdentity (typedSegmentOriginalRoot typed)) (preparedRootIdentity (typedItemRoot item)) ordinal
            pure (generation,captures)
          validateDependencyEvidence (preparedFreshDependencies prepared)
          when (null emitted) (fail "typed segment emitted no items")
          extended <- retainProgramProducts directory prepared sharedProducts sourceOwner scope
          retainedImports <- retainProgramSourceImports (programSourceImports state) prepared
            (certifiedFinalizedArtifacts sharedProducts) extended
          let completedState = recordTypedSegment finalized typedPlan expressionObservations captureSignatures rendered
                state {programExact=extended,programSourceImports=retainedImports}
          next <- foldM (\current (generation,captures) ->
            addProgramValue stagingRoot generation captures current) completedState emitted
          modifyIORef' batches (++ [batch])
          pure next

    scopeValues = maybe [] checkedValueInterfaces . scopeCheckedCell
    extractPlannedFingerprint inventory = plannedInterfaceFingerprint inventory
    plannedSourceFromDirectory plan = concatMap cellAnalysisSource (cellPlanItems plan)

encodeTypedSegmentPlan :: TypedSegmentPlan -> Encoding
encodeTypedSegmentPlan plan =
  encodeListLen 4 <> text (typedSegmentPlanDigest plan) <> text (typedSegmentReservationDigest plan)
    <> text (typedSegmentPlanRoot plan)
    <> encodeListLen (fromIntegral (length (typedSegmentPlanItems plan)))
    <> foldMap encodeItem (typedSegmentPlanItems plan)
  where
    text = encodeString . T.pack
    encodeItem item = encodeListLen 8 <> encodeWord64 (fromIntegral (plannedItemOrdinal item))
      <> text (plannedItemEntry item) <> encodeWord64 (plannedItemGeneration item)
      <> case plannedItemBody item of
        ActionItem step probe marker captures ->
          text "action" <> text step <> text probe <> text marker <> names captures
        LetItem marker captures -> text "let" <> text "" <> text "" <> text marker <> names captures
        ObservationItem probe observation -> text "observation" <> text "" <> text probe <> text observation <> names []
    names values = encodeListLen (fromIntegral (length values)) <> foldMap text values

typedEntryOriginalOrdinal :: CertifiedOriginalProducts -> SymbolIdentity -> IO Word32
typedEntryOriginalOrdinal certified entry = case
  [Execution.projectedOriginalOrdinal group | original <- certifiedOriginalProducts certified
    , let (unit,owner,_,groups) = moduleProductInput original
    , unit == symbolUnit entry, owner == symbolModule entry
    , group <- groups, entry `elem` Execution.projectedBinders group] of
    [ordinal] -> pure ordinal
    _ -> fail "typed item entry has no unique compiler-issued original group"

writeTypedItemReceipt :: FilePath -> ExactScope -> CheckedItemAdmission -> String
  -> TypedSegmentPlan -> SymbolIdentity -> SymbolIdentity -> Word32 -> IO ()
writeTypedItemReceipt root scope admission source plan original entry ordinal = do
  let text = encodeString . T.pack
      identity value = encodeListLen 3 <> encodeString (symbolUnit value)
        <> encodeString (symbolModule value) <> encodeString (symbolOccurrence value)
      receipt = encodeListLen 9 <> text "TPEXACTITEM" <> text "2"
        <> text (scopeRequestSha256 scope) <> text (itemAdmissionDigest admission)
        <> text (itemCellReceiptDigest admission) <> encodeWord64 (itemIndex admission)
        <> text (shaHex (TE.encodeUtf8 (T.pack source))) <> text "tidepool-checked-recipe-2"
        <> encodeListLen 4 <> text (typedSegmentPlanDigest plan) <> identity original <> identity entry
        <> encodeWord32 ordinal
  unless (symbolUnit original == symbolUnit entry && symbolModule original == symbolModule entry
    && symbolNamespace original == "value" && symbolNamespace entry == "value"
    && isNothing (symbolRecordParent original) && isNothing (symbolRecordParent entry))
    (fail "typed segment roots leave their ordinary value owner")
  BS.writeFile (root </> "checked-item.cbor") (toStrictByteString receipt)

programPurpose :: ProgramCellState -> CompilePurpose -> CompilePurpose
programPurpose state = withProgramSourceImports (programSourceImports state) . replacePurpose
  where
    replacePurpose purpose = case purpose of
      ParsedImportSelection intents inner -> ParsedImportSelection intents (replacePurpose inner)
      GeneratedScaffoldCompile recipe inner -> GeneratedScaffoldCompile recipe (replacePurpose inner)
      GeneratedInstanceCheck recipe inner -> GeneratedInstanceCheck recipe (replacePurpose inner)
      TypedSegmentCompile plan operations inner -> TypedSegmentCompile plan operations (replacePurpose inner)
      OriginalDeclarationCompile -> ProgramItemCompile True [] (programOriginals state) (selectedProgramValues (programValues state))
      CheckedItemCompile annotations _ values -> ProgramItemCompile False annotations (programOriginals state) values
      other -> other

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
    imports = [LocatedImport (CellSourceSpan 1 1 1 1) ("import " ++ owner) RetainedGeneratedImport [mkModuleName owner]
      | ((_,owner),_) <- original]
      ++ [LocatedImport (CellSourceSpan 1 1 1 1) ("import " ++ completedValueModule value
        ++ " (" ++ intercalate ", " (map (renderImportBinder . fst) (completedValueBinders value)) ++ ")") RetainedGeneratedImport [mkModuleName (completedValueModule value)] | value <- values]

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
      selection = CompletedValueImport "main" (bbModule firstBinder) path digest
        [(bbName binder,bbVarId binder) | binder <- binders, not ("__tidepoolMetadata" `isPrefixOf` bbName binder)]
  lexicalRequirements <- programLexicalRequirements exact [] requirements
  extended <- extendCheckedValueScope exact
    (artifact,path ++ ".packages",shaHex packages) lexicalRequirements >>= either fail pure
  let retained = foldr (\binder -> Map.insert
          (SymbolIdentity "main" (T.pack (bbModule binder)) "value" (T.pack (bbName binder)) Nothing) generation)
        (programRetained state) binders
  pure state { programExact = extended, programValues = programValues state ++ [selection]
    , programRetained = retained }

-- Refine generated instances in the original declaration's typed environment.
-- The successful frontend's prepared interface supplies the declaration's
-- native product and its retained type evidence.
prepareOriginalCellDeclaration
  :: Compiler -> RecoveryCaches -> WorkerRequest -> String -> FilePath -> Maybe SessionScope
  -> ExactScope -> CheckedCellAdmission -> CellSourcePlan
  -> IO (CellSourcePlan, PlannedDeclaration, PlannedDeclarationInventory, ExactScope,
         (PreparedPipelineResult, CertifiedOriginalProducts))
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
      compileOriginal plan = do
        _ <- writeOriginal plan
        compiler (PreparedProducts (requestModuleCandidates args))
          (Map.keysSet (requestRetainedGenerations args))
          (withSourceImportIntents (cellPlanPrologue plan)
            (GeneratedInstanceCheck (cellGeneratedInstanceRecipe plan) OriginalDeclarationCompile)) scope sourcePath
          (requestIncludes args) (requestBuildProductsDir args)
  createDirectoryIfMissing True directory
  (finalized, prepared) <- checkCellInstances compileOriginal initial
  original <- planned finalized
  let result = pprPipelineResult prepared
      environment = prHscEnv result
      binds = prBinds result
  inventory <- certifyPlannedDeclaration original environment >>= either fail pure
  originalInterfaces <- newPreparedOriginalInterfaceArtifacts prepared directory
  (artifacts, productContext) <- prepareArtifacts originalInterfaces directory caches prepared
    ["__result"] [] (requestRetainedGenerations args) []
  writePreparedSidecars SeparateYieldSites directory binds (prTyCons result)
    Nothing (map T.pack (prWarnings result)) artifacts
  writePreparedArtifacts directory artifacts
  certified <- writeRequestProducts args originalInterfaces directory prepared productContext artifacts
  let products = certifiedOriginalProducts certified
  supportScope <- retainProgramProducts directory prepared certified reserved exact
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
  localProof <- maybe (fail "planned original declaration lacks captured finalization") pure
    (Map.lookup (unit,reserved) (finalizedLocalAdmissions (certifiedFinalizedArtifacts certified)))
  when (isNothing (localFinalizedCore localProof))
    (fail "planned original declaration lacks its finalized Core")
  lexicalRequirements <- programSourceRequirements prepared unit reserved >>= programLexicalRequirements supportScope []
  let (interface,packagesPath,packagesSha) = localFinalizedInterface localProof
  unless (exactSha256 interface == shaHex interfaceBytes)
    (fail "planned native product differs from its finalized interface")
  packageBytes <- BS.readFile packagesPath
  unless (shaHex packageBytes == packagesSha)
    (fail "planned original declaration package capture changed")
  nativeEvidence <- either fail pure (admitLocalNativeDeclaration original inventory localProof)
  let originalProduct = ExactProduct unit reserved
        (exactProgramProductVersion exact unit reserved (plannedSource original) interfaceBytes originalBytes packageBytes)
        (shaHex interfaceBytes) (shaHex originalBytes) productPath
        (map originalGroupFromProjected originalGroups')
      text = encodeString . T.pack
      receipt = encodeListLen 8 <> text "TPEXACTDECL" <> text "1"
        <> text (scopeRequestSha256 exact) <> text reserved <> text (plannedSource original)
        <> text (shaHex interfaceBytes) <> text (renderPlannedDeclarationInventory inventory)
        <> text "planned-declaration"
  extended <- extendExactScopeGeneration supportScope
    [((interface,packagesPath,packagesSha),LocalNativeDeclarationEvidence nativeEvidence)]
    [originalProduct] [((unit,reserved),lexicalRequirements)] >>= either fail pure
  BS.writeFile (outDir </> "planned-declaration.cbor") (toStrictByteString receipt)
  pure (finalized, original, inventory, extended, (prepared,certified))
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
  :: FilePath -> PreparedPipelineResult
  -> CertifiedOriginalProducts -> String -> ExactScope -> IO ExactScope
retainProgramProducts directory prepared certified target initial = do
  selected <- either fail pure (extendSourceSelectedOriginals
    (preparedExactCompilation prepared >>= compilationSourceSelection) initial)
  finalized <- foldM retainInterface (selected,[],[],[]) (Map.toAscList localInterfaces)
  (cached,additions,pendingProducts,pendingLexical) <- foldM retainCached finalized (zip [0::Int ..] (pprAcceptedCandidates prepared))
  admitted <- extendExactScopeGeneration cached additions pendingProducts pendingLexical >>= either fail pure
  promoted <- foldM retain admitted (zip [0::Int ..] products)
  let parcels = mapMaybe candidateExecutionSources (pprAcceptedCandidates prepared)
  inherited <- either throwIO pure
    (extendExactExecutionSources (concatMap fst parcels) (map snd parcels) promoted)
  case certifiedExecutionSource certified of
    ExactExecutionSourceAvailable graph -> do
      let prospective = [ExecutionSourceRef (executionOwnerIdentity owner) (executionGraphSha256 graph)
            | owner <- executionGraphOwners graph, executionOwnerFresh owner
            , executionModule (executionOwnerIdentity owner) /= target]
      references <- either throwIO pure (executionSourceProspectiveReferences
        (graph : scopeExecutionGraphs inherited) (scopeExecutionOwners inherited) prospective)
      extended <- either throwIO pure
        (extendExactExecutionSourcesWithinBudget [graph] references inherited)
      pure (fromMaybe inherited extended)
    ExactExecutionSourceUnavailable _ -> pure inherited
    OrdinaryExecutionSource -> fail "exact program products lack exact source recipe outcome"
  where
    localInterfaces = Map.filterWithKey (\(_,owner) _ -> owner /= target)
      (finalizedLocalAdmissions (certifiedFinalizedArtifacts certified))
    products = [product' | product' <- certifiedOriginalProducts certified
      , let (_, owner, _, _) = moduleProductInput product', T.unpack owner /= target]
    supportOwners = [(candidateUnit candidate,candidateModule candidate)
      | candidate <- pprAcceptedCandidates prepared]
      ++ Map.keys localInterfaces
    retainInterface (scope,additions,stagedProducts,stagedLexical) (key,proof) = do
      canonical <- maybe (fail "supporting source original lacks complete canonical proof") pure
        (Map.lookup key (certifiedSourceOriginals certified))
      let row@(interface,_,packagesSha) = localFinalizedInterface proof
          existing = [current | current@(artifact,_,_) <- selectedInterfacesOf scope additions
            , (exactUnit artifact,exactModule artifact) == key]
      lexicalRequirements <- programSourceRequirements prepared (fst key) (snd key)
        >>= programLexicalRequirements scope supportOwners
      case existing of
        [] -> pure (scope, additions ++ [(row,ModuleInterfaceEvidence canonical)],
          stagedProducts,stagedLexical ++ [(key,lexicalRequirements)])
        [(old,_,oldPackagesSha)]
          | exactSha256 old == exactSha256 interface
          , exactRequirements old == exactRequirements interface
          , oldPackagesSha == packagesSha
          , lookup key (scopeLexical scope ++ stagedLexical) == Just lexicalRequirements
          , Just (ModuleInterfaceEvidence oldCanonical) <- Map.lookup key (selectedEvidenceOf scope additions)
          , canonicalCertificateSha256 oldCanonical == canonicalCertificateSha256 canonical -> pure (scope,additions,stagedProducts,stagedLexical)
        _ -> fail "fresh finalization conflicts with an admitted original owner"
    retainCached (scope,additions,stagedProducts,stagedLexical) (index, candidate) = do
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
      selectedInterfaces <- foldM selectCandidateInterface (selectedInterfacesOf scope additions)
        (pprAcceptedCandidates prepared)
      proof <- validateCandidateCanonicalInterfaceProof (scopeProducerSha256 scope)
        selectedInterfaces candidate >>= either fail pure
      let requirements = candidateInterfaceRequirements candidate
      lexicalRequirements <- programSourceRequirements prepared unit owner >>= programLexicalRequirements scope supportOwners
      let groups = map originalGroupFromCandidate (candidateGroups candidate)
          existingInterfaces = [(artifact,packages,sha)
            | (artifact,packages,sha) <- selectedInterfacesOf scope additions
            , (exactUnit artifact,exactModule artifact) == key]
          existingProducts = [original | original <- scopeProducts scope ++ stagedProducts
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
          pure (scope,additions ++ [((interface,packagesPath,candidatePackageImportsSha256 candidate),ModuleInterfaceEvidence proof)],
            stagedProducts ++ [original],stagedLexical ++ [(key,lexicalRequirements)])
        ([(interface,packagesPath,packagesSha)],[original])
          | lookup key (scopeLexical scope ++ stagedLexical) == Just lexicalRequirements
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
              case Map.lookup key (selectedEvidenceOf scope additions) of
                Just (ModuleInterfaceEvidence retained)
                  | canonicalCertificateSha256 retained == canonicalCertificateSha256 proof -> pure (scope,additions,stagedProducts,stagedLexical)
                _ -> fail "cached source product conflicts with retained canonical evidence"
        _ -> fail "cached source product conflicts with an admitted original owner"
    selectedInterfacesOf scope additions = scopeInterfaces scope ++ map fst additions
    selectedEvidenceOf scope additions = Map.union
      (Map.fromList [((exactUnit interface,exactModule interface),evidence)
        | ((interface,_,_),evidence) <- additions]) (scopeInterfaceEvidence scope)
    selectCandidateInterface interfaces candidate = do
      let key = (candidateUnit candidate,candidateModule candidate)
          row = (ExactIfaceArtifact (candidateUnit candidate) (candidateModule candidate)
            (candidateInterface candidate) (candidateInterfaceSha256 candidate)
            (candidateInterfaceRequirements candidate),candidatePackageImports candidate,
            candidatePackageImportsSha256 candidate)
      case [existing | existing@(interface,_,_) <- interfaces
          , (exactUnit interface,exactModule interface) == key] of
        [] -> pure (interfaces ++ [row])
        [(interface,_,packageSha)]
          | exactSha256 interface == candidateInterfaceSha256 candidate
          , exactRequirements interface == candidateInterfaceRequirements candidate
          , packageSha == candidatePackageImportsSha256 candidate -> pure interfaces
        _ -> fail "accepted candidate conflicts with the selected canonical interface closure"
    retain scope (index, originalProduct) = do
      let (unitText,ownerText,interfaceBytes,groups) = moduleProductInput originalProduct
          unit = T.unpack unitText
          owner = T.unpack ownerText
          key = (unit,owner)
      forM_ [original | original <- scopeProducts scope
          , (originalUnit original,originalModule original) == key] $ \original ->
        fail ("new native product replaces an admitted original owner: " ++ show key
          ++ "; admitted ordinals=" ++ show (map originalOrdinal (originalGroups original))
          ++ "; offered ordinals=" ++ show (map Execution.projectedOriginalOrdinal groups))
      (sourceDigest,(interface,packagesPath,packagesSha)) <- case Map.lookup key (certifiedRetainedOriginals certified) of
        Just original -> do
          unless (case Map.lookup key (scopeInterfaceEvidence scope) of
              Just (ModuleInterfaceEvidence retained) ->
                canonicalCertificateSha256 retained == canonicalCertificateSha256 original
              _ -> False) (fail "prepared retained original changed canonical authority")
          row <- case [row | row@(artifact,_,_) <- scopeInterfaces scope
              , (exactUnit artifact,exactModule artifact) == key] of
            [row] -> pure row
            _ -> fail "prepared retained original has no unique admitted interface"
          pure (canonicalSourceSha256 original,row)
        Nothing -> do
          proof <- maybe (fail "supporting native product lacks captured finalization") pure
            (Map.lookup key localInterfaces)
          when (isNothing (localFinalizedCore proof))
            (fail "supporting native product lacks finalized Core")
          pure (localFinalizedSourceSha256 proof,localFinalizedInterface proof)
      unless (exactSha256 interface == shaHex interfaceBytes)
        (fail "supporting native product differs from finalized interface")
      packageBytes <- BS.readFile packagesPath
      unless (shaHex packageBytes == packagesSha)
        (fail "supporting native package capture changed")
      let productBytes = moduleProductBytes originalProduct
      version <- case Map.lookup key (certifiedRetainedOriginals certified) of
        Just _ -> maybe (fail "retained native product lacks its certified demand graph identity") pure
          (Map.lookup key (certifiedRetainedNativeVersions certified))
        Nothing -> pure (exactProgramProductVersionFromDigest scope unit owner sourceDigest interfaceBytes productBytes packageBytes)
      let stem = directory </> "retained-original-" ++ show index
          productPath = stem ++ ".product.cbor"
          original = ExactProduct unit owner version
            (shaHex interfaceBytes) (shaHex productBytes) productPath
            (map originalGroupFromProjected groups)
      BS.writeFile productPath productBytes
      extendExactScopeGeneration scope [] [original] [] >>= either fail pure

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
  source <- programSourceRequirements prepared unit owner
  iface <- case Map.lookup (mkModuleName owner) (pprProductInterfaces prepared) of
    Just value | unitString (moduleUnit (mi_module value)) == unit -> pure value
    _ -> fmap fst <$> readExactInterface (prHscEnv (pprPipelineResult prepared))
      (mkModule (stringToUnit unit) (mkModuleName owner)) >>= either (fail . show) pure
  pure (nub (source ++ homeInterfaceUsageOwners (prHscEnv (pprPipelineResult prepared)) iface))

programSourceRequirements :: PreparedPipelineResult -> String -> String -> IO [(String, String)]
programSourceRequirements prepared unit owner =
  either fail pure (preparedHomeRequirements prepared unit owner)

exactProgramProductVersion :: ExactScope -> String -> String -> String -> BS.ByteString -> BS.ByteString -> BS.ByteString -> String
exactProgramProductVersion scope unit owner source =
  exactProgramProductVersionFromDigest scope unit owner (shaHex (TE.encodeUtf8 (T.pack source)))

originalDeclarationWrapper :: String -> IO String
originalDeclarationWrapper template = do
  let marker = "\n__tidepoolInEffectRow ::"
      (prefix, remaining) = T.breakOn (T.pack marker) (T.pack template)
  when (T.null remaining) (fail "original declaration requires canonical whole-cell recipe")
  let stripped = T.replace "{{CELL_PRAGMAS}}" "" prefix
  when ("{{" `T.isInfixOf` T.replace "{{CELL_IMPORTS}}" "" stripped) (fail "original declaration wrapper has an unknown placeholder")
  pure (T.unpack stripped ++ "\n{{TURN}}\n__result :: ()\n__result = ()\n")

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
