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
  ( evaluate, try, throwIO, mask, SomeException
  , IOException )
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
import GHC.Types.Name (nameOccName)
import GHC.Types.Id (idName)
import GHC.Types.Name.Occurrence (occNameString)
import qualified Data.Text as T
import qualified Data.Text.Encoding as TE

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
import Tidepool.CompilerRequest
import Tidepool.GhcPipeline
  ( PipelineSelection(..), PreparedPipelineResult, pprPipelineResult, pprModules, pprProductInterfaces
  , pprPackageImports, CheckedEnvironmentResult(..)
  , preparedFreshDependencies, preparedExactCompilation, retainProgramSourceImports
  , CompilePurpose(..), withSourceImportIntents, prBinds, prHscEnv
  , prCapturedType, prWarnings, prTargetTcGblEnv, CompilerScope
  , withResidentCompilerScopes, checkCellInstances
  , cellGeneratedInstanceRecipe, cellExpressionEvidence, cellCheckedBinderSignatures
  , satisfiesCapturedConstraint, activationPreviewInputType )
import Tidepool.ExecutionEncode (moduleProductInput, moduleProductBytes)
import Tidepool.CompilerProducts
  ( CertifiedOriginalProducts, certifiedOriginalProducts, certifiedFinalizedArtifacts
  , PreparedProductContext
  , newPreparedOriginalInterfaceArtifacts, writeCertifiedProductsKeepingWithOriginals
  , writeCertifiedSegmentProducts, writeCertifiedSegmentItemProducts
  , stagedOriginalEntryOrdinal, stagedFinalizedArtifacts
  , retainStagedProgramProducts
  , exactProgramProductVersionFromDigest, retainProgramProducts, programLexicalRequirements, programSourceRequirements )
import Tidepool.ExecutionProjection (preparedRootIdentity)
import Tidepool.ExecutionSchema (SymbolIdentity(..))
import qualified Tidepool.EffectSchema
import Tidepool.PreparedStg
  ( pmModule, pmIdentityResolver )
import Tidepool.CompileInput (writeCompileInputProof)
import Tidepool.CertifiedProducts (homeInterfaceUsageOwners)
import Tidepool.FinalizedModuleArtifacts
  ( finalizedLocalAdmissions
  , localFinalizedInterface, localFinalizedCore )
import Tidepool.ExactHydration (ExactIfaceArtifact(..), CheckedTemplateImports(..), OriginalInterfaceArtifacts
  , generatedActivationPreviewRecipe
  , RenderedProtectedTemplateImports
  , captureProtectedTemplateImports, captureProtectedTemplateImportsAt, renderProtectedTemplateImports
  , generatedScaffoldRecipeWithProtectedImports, generatedCheckingTemplateRecipeWithProtectedImports
  , generatedTypedSegmentRecipeWithProtectedImports)
import Tidepool.DeclarationJoin
  ( DeclarationOperation(..), readDeclarationOperation, validateDeclarationJoin
  , HostBindingInterfaceInput(..), encodeHostBindingInterface, encodeBindingInterfacePurpose
  , DeclarationExport(..), ExportIdentity(..), ExportNamespace(..)
  , renderDeclarationJoinOutcome, inspectDeclarationArtifacts
  , renderDeclarationInventoryOutcome )
import qualified Tidepool.WorkerServer as WorkerServer
import Tidepool.DiagJson
  ( Diag, SourceRejection(..), InputRejection(..) )
import Tidepool.CheckedAdmission
  ( validateCheckedCellAdmission, validateCheckedItemAdmission )
import Tidepool.CheckedRecipe
  ( checkedItemCompilePurpose, checkedRecipeSourceWithLineOffset, replaceRecipeMarker, writeCheckedItemReceipt )
import Tidepool.ExtractUtil (shaHex, trySynchronous)
import Tidepool.RequestCompletion
  ( Completion, PhaseSink, BodyOutcome(..), CleanupStage(CompilerScopeRestore), captureCompletion
  , flattenCompletion, completionResult, completionFailures, appendCleanupFailures, asCleanupFailures )
import Tidepool.WorkerDiagnostics
  ( throwCellSplitError, sourceFailureDiagnostics )
import Tidepool.ExtractRequest (RequestOperation(..), WorkerRequest(..), workerRequestFromArgv, workerRequestFlag)
import Tidepool.Introspection (encodeInspectionResults, runInspectionGhc, withInspectionWork)
import Tidepool.InspectionRunner (isInspectionTypeQuery, runInspectionRequests)
import Tidepool.ExactScope
  ( ExactScope, scopeRequestSha256, scopeSemanticSha256, ExactProduct(..)
  , ActivationPreviewAdmission(..), scopeActivationPreview
  , scopeCheckedCell, scopeCheckedItem
  , originalGroupFromProjected
  , CheckedCellAdmission(..), CheckedItemAdmission(..), CheckedItemPurpose(..), PlannedCellAdmission(..), PlannedCellSlot(..)
  , ExactInterfaceEvidence(..)
  , revalidateExactScope, extendExactScopeGeneration, extendCheckedValueScope
  )
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
  ( validateDependencyEvidence, writeDependencyEvidence )

renderAsksJson :: [Tidepool.EffectSchema.YieldSite] -> String
renderAsksJson sites = "[" ++ intercalate "," (map renderAskJson sites) ++ "]"

-- | Serve one typed request, or answer the role probe before compiler setup.
-- For requests, stdout contains exactly one diagnostics document; stderr is
-- the human-readable channel.
main :: IO ()
main = do
  rawWorkerRequest <- getArgs
  if rawWorkerRequest == ["--print-tidepool-role-v1"]
    then putStrLn "tidepool-extractor-role-v1 worker" >> exitWith ExitSuccess
  else if rawWorkerRequest == ["--print-worker-request-flag"]
    then putStrLn workerRequestFlag >> exitWith ExitSuccess
  else if rawWorkerRequest == ["--worker-loop-v2"]
    then do
      hSetBinaryMode stdin True
      hSetBinaryMode stdout True
      withResidentCompilerScopes [] $ \runRequest ->
        WorkerServer.runWorkerLoop $ \serveTransaction ->
          runRequest (pure ()) $ \scope ->
            serveTransaction (\sink cwd argv ->
              setCurrentDirectory cwd >> runWorkerInvocation sink scope argv)
    else do
      hSetEncoding stdout utf8
      -- Direct transport boots the same compiler owner once for all helpers.
      result <- WorkerServer.runDirectRequest (runDirectInvocation rawWorkerRequest)
      exitWith result

-- Retain the issued request result while the outer GHC scope and interpreter
-- release. A failure before issuance remains an independent operation failure.
runDirectInvocation :: [String] -> PhaseSink -> IO (Completion [Diag])
runDirectInvocation raw sink = do
  issued <- newIORef Nothing
  outer <- captureCompletion $ withResidentCompilerScopes [] $ \runRequest ->
    runRequest (pure ()) $ \scope -> mask $ \restore -> do
      completed <- restore (runWorkerInvocation sink scope raw)
      writeIORef issued (Just completed)
      pure completed
  known <- readIORef issued
  pure $ case known of
    Nothing -> flattenCompletion outer
    Just completed -> appendCleanupFailures completed $ case completionResult outer of
      BodySucceeded _ -> completionFailures outer
      _ -> asCleanupFailures CompilerScopeRestore outer

-- Direct and resident transports decode the same versioned worker payload.
runWorkerInvocation :: PhaseSink -> CompilerScope -> [String] -> IO (Completion [Diag])
runWorkerInvocation sink compilerScope raw = case workerRequestFromArgv raw of
  Left failure -> captureCompletion (throwIO (InvalidWorkerRequest failure))
  Right Nothing -> captureCompletion (throwIO (InvalidWorkerRequest "worker requires a versioned request"))
  Right (Just request) -> runParsedInvocation sink compilerScope request

runParsedInvocation :: PhaseSink -> CompilerScope -> WorkerRequest -> IO (Completion [Diag])
runParsedInvocation sink compilerScope parsed = flattenCompletion <$> captureCompletion (do
  timing <- readTimingEnabled
  withBoundCompilerRequest sink compilerScope parsed (dispatch timing))

-- Mode selection, normalized arguments and exact admission come from one owner.
dispatch :: Bool -> BoundCompilerRequest -> IO [Diag]
dispatch timing bound =
  let args = boundWorkerRequest bound
  in case boundRequestOperation bound of
    DeclarationOperation manifest output -> runDeclarationOperation bound manifest output
    _ -> case requestFiles args of
      [] -> fail "worker request contains no input"
      (file : _)
        | isJust (requestInspectTypeBatch args)
          && not (length (requestInspections args) > 1 && all isInspectionTypeQuery (requestInspections args))
          -> fail "inspection type batch requires at least two type queries and no other query kinds"
        | otherwise -> case boundRequestOperation bound of
            ActivationPreviewOperation -> runActivationPreviewMode bound file
            SourceCheckOperation -> runSourceCheckMode bound file
            CellPlanOperation -> runCellPlanMode (boundParserFlags bound) args file
            CellOperation -> runCellMode bound file
            ClassificationOperation -> runClassifyMode (boundParserFlags bound) timing args
            InspectionOperation -> runInspectionMode bound file
            TurnOperation -> runTurnMode bound file
            SourceOperation -> timePhase timing "total" (processFile bound timing file)

-- Compile a pure function over an already mounted input. No value interface
-- or authored completion is issued by either the opaque probe or final pass.
runActivationPreviewMode :: BoundCompilerRequest -> FilePath -> IO [Diag]
runActivationPreviewMode bound path = do
  let args = boundWorkerRequest bound
      parserFlags = boundParserFlags bound
  timing <- readTimingEnabled
  lastAttempt <- newIORef Nothing
  result <- timePhase timing "total" $ trySynchronous $ do
    exact <- requireArg "--session-artifacts" (boundRequestExactScope bound)
    admission <- maybe (throwIO CheckedPurposeMismatch) pure (scopeActivationPreview exact)
    let readTemplate = withBinaryFile path ReadMode $ \handle -> do
          bytes <- BS.hGet handle ((32 * 1024 * 1024) + 1)
          when (BS.length bytes > 32 * 1024 * 1024) (fail "activation preview template exceeds source bound")
          pure bytes
    originalBytes <- readTemplate
    unless (shaHex originalBytes == previewTemplateSha256 admission)
      (fail "activation preview template differs from its protected offer")
    original <- either (fail . show) (pure . T.unpack) (TE.decodeUtf8' originalBytes)
    selectedFlags <- templateParserFlags parserFlags original >>= either throwCellSplitError pure
    let outDir = fromMaybe (takeDirectory path </> takeBaseName path ++ "_cbor") (requestOutDir args)
        opaque = "(TidepoolScaffoldText.pack \"<opaque value>\\nUse the input type to select fields or apply sessionInput.\", False)"
        basePurpose = HostActivationPreviewCompile (previewInputSignature admission)
        render body = do
          source <- replaceRecipeMarker "{{ACTIVATION_PREVIEW}}" body original
          let owner = generatedScaffoldModuleName
                ["activation-renderer-scaffold-v1", previewOriginalContextDigest admission, source]
          protected <- either fail pure (renameScaffoldModuleHeader selectedFlags owner original)
          named <- replaceRecipeMarker "{{ACTIVATION_PREVIEW}}" body protected
          (_, moduleName', modulePath) <- writeSplicedModule selectedFlags outDir lastAttempt named
          recipe <- generatedActivationPreviewRecipe parserFlags (previewOriginalInterfaces admission)
            (previewOriginalTarget admission) protected named modulePath moduleName' >>= either fail pure
          pure (named, modulePath, GeneratedScaffoldCompile recipe basePurpose)
    (_, probePath, probePurpose) <- render opaque
    probe <- compileBoundRequest bound CheckedEnvironment Set.empty probePurpose (Just (scopeFromWorkerRequest args))
      probePath
    input <- either fail pure (activationPreviewInputType (crTargetTcGblEnv probe))
    rendered <- satisfiesCapturedConstraint (crHscEnv probe) (crTargetTcGblEnv probe)
      "__tidepoolActivationConstraint" input
    (source, modulePath, purpose) <- render (if rendered
      then "TidepoolInspection.workbenchActivationDisplay " ++ show (previewBudget admission) ++ " __activationInput"
      else opaque)
    completion <- compilePreparedRequest bound (PreparedProducts (requestModuleCandidates args)) Set.empty purpose
      (Just (scopeFromWorkerRequest args)) modulePath
    let prepared = preparedCompilationResult completion
        compiled = pprPipelineResult prepared
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
      originalInterfaces outDir completion [preparedScaffoldTargetName]
      (standardAuxiliaryRoots binds) Map.empty [])
      :: IO (Either PreviewOriginalDependenciesUnavailable ([PreparedArtifact], Maybe PreparedProductContext))
    validateDependencyEvidence (preparedFreshDependencies prepared)
    revalidateExactScope environment exact >>= either fail pure
    templateBytes <- readTemplate
    unless (templateBytes == originalBytes) (fail "activation preview template changed during compilation")
    let text = encodeString . T.pack
    case projection of
      Left _ -> BS.writeFile (outDir </> "activation-preview-unavailable.cbor") $ toStrictByteString
        (encodeListLen 6 <> text "TPEXACTACTIVATIONRENDERERUNAVAILABLE1" <> text "1"
          <> text (scopeRequestSha256 exact) <> text (previewOriginalContextDigest admission)
          <> text (previewTemplateSha256 admission)
          <> encodeBytes (previewInputWitness admission))
      Right (artifacts, productContext) -> do
        unless (all (null . paYieldSites) artifacts) (fail "pure activation preview emitted suspension sites")
        writePreparedSidecars InlineYieldSites outDir binds
          (T.pack <$> prCapturedType compiled) (map T.pack (prWarnings compiled)) artifacts
        writePreparedArtifacts outDir artifacts
        void $ writeRequestProducts args originalInterfaces outDir prepared productContext artifacts
        outFile <- requireArg "--turn-out" (requestTurnOut args)
        BS.writeFile outFile (encodeTurnOut (TExpr 0 (concatMap paYieldSites artifacts) (T.pack source)))
        BS.writeFile (outDir </> "activation-preview.cbor") $ toStrictByteString
          (encodeListLen 7 <> text "TPEXACTACTIVATIONRENDERER1" <> text "1"
            <> text (scopeRequestSha256 exact) <> text (previewOriginalContextDigest admission)
            <> text (shaHex (TE.encodeUtf8 (T.pack source)))
            <> encodeBytes witnessBytes <> text (if rendered then "rendered" else "opaque"))
  case result of
    Left _ -> readIORef lastAttempt >>= mapM_ (\(output, source) -> do
      void (try (writeFile output source) :: IO (Either IOException ())))
    Right _ -> pure ()
  either throwIO (const (pure [])) result

runSourceCheckMode :: BoundCompilerRequest -> FilePath -> IO [Diag]
runSourceCheckMode bound path = do
  let args = boundWorkerRequest bound
  checked <- trySynchronous $ do
    compiled <- compileBoundRequest bound (maybe CheckedEnvironment CheckedEnvironmentProducts (requestModuleCandidates args))
      Set.empty GeneralCompile Nothing path
    pure (crWarnings compiled)
  either throwIO pure checked

runDeclarationOperation :: BoundCompilerRequest -> FilePath -> FilePath -> IO [Diag]
runDeclarationOperation bound manifest out = do
  result <- trySynchronous $ do
    operation <- readDeclarationOperation manifest
    withBoundExactInterfaceTransaction bound $ \operations ->
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
  either throwIO (const (pure [])) result

runInspectionMode :: BoundCompilerRequest -> FilePath -> IO [Diag]
runInspectionMode bound _path = do
  let args = boundWorkerRequest bound
  res <- trySynchronous $ withInspectionWork $ \work -> do
    out <- maybe (fail "inspection request is missing its output path") pure (requestInspectOut args)
    let scope = if hasSessionScope args then Just (scopeFromWorkerRequest args) else Nothing
        compile purpose path = compileBoundRequest bound CheckedEnvironment Set.empty purpose scope path

        inspect successful requests = runBoundGhc bound $ runInspectionGhc work
          (crHscEnv successful)
          (crTargetTcGblEnv successful)
          (crTargetRdrEnv successful)
          (crInspectionProbes successful) requests
    results <- runInspectionRequests args compile inspect
    BS.writeFile out (encodeInspectionResults results)
  either throwIO (const (pure [])) res


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
  :: BoundCompilerRequest -> Bool -> FilePath -> IO [Diag]
processFile bound timing path = do
  let args = boundWorkerRequest bound
  let mOutDir = requestOutDir args
      mTarget = requestTarget args
  hPutStrLn stderr $ "Processing: " ++ path
  res <- trySynchronous $ do
    -- Multi-target extraction can inject stable session values without
    -- becoming a session bind/reference operation.
    let scope = if hasSessionScope args then Just (scopeFromWorkerRequest args) else Nothing
    let purpose = if requestCertifyHomeProducts args
          then CertifyHomeProductsCompile else GeneralCompile
    completion <- compilePreparedRequest bound (PreparedProducts (requestModuleCandidates args)) (Map.keysSet (requestRetainedGenerations args)) purpose scope path
    let prepared = preparedCompilationResult completion
        result = pprPipelineResult prepared
    let binds = prBinds result
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
    output <- prepareArtifactsWithIntent
      (preparedOutputIntent args)
      originalInterfaces outDir completion preparedTargets
      (standardAuxiliaryRoots binds) (requestRetainedGenerations args) []
    let preparedArtifacts = case output of
          NativeOutputs artifacts _ -> artifacts
          PortableOutputs artifacts -> artifacts
    if null preparedArtifacts
      then ioError (userError "prepared extraction requires --target or --targets")
      else timePhase timing "prepared_sidecars" $ writePreparedSidecars SeparateYieldSites outDir binds mCapturedTy warnTexts preparedArtifacts

    timePhase timing "prepared_write" $ writePreparedArtifacts outDir preparedArtifacts
    case output of
      NativeOutputs _ productContext -> void $ timePhase timing "module_products" $
        writeRequestProducts args originalInterfaces outDir prepared productContext preparedArtifacts
      PortableOutputs _ -> do
        _ <- writeDependencyEvidence outDir (preparedFreshDependencies prepared)
        writeRequestInputProof args outDir prepared
        BS.writeFile (outDir </> "portable-output.cbor") "TPPORTABLEOUTPUT1\n"

  either throwIO (const (pure [])) res

-- Every completed worker product carries its original input evidence. Exact
-- and session requests use their separate retained authority instead.
writeRequestProducts
  :: WorkerRequest -> OriginalInterfaceArtifacts -> FilePath -> PreparedPipelineResult -> Maybe PreparedProductContext
  -> [PreparedArtifact] -> IO CertifiedOriginalProducts
writeRequestProducts args originalInterfaces outDir prepared productContext preparedArtifacts = do
  products <- writeCertifiedProductsKeepingWithOriginals (requestIncludes args) originalInterfaces outDir prepared productContext
    [(paTarget artifact, paProgram artifact) | artifact <- preparedArtifacts]
  writeRequestInputProof args outDir prepared
  pure products

writeRequestInputProof :: WorkerRequest -> FilePath -> PreparedPipelineResult -> IO ()
writeRequestInputProof args outDir prepared =
  when (not (requestCell args) && not (requestActivationPreview args)
      && null (requestInjectVals args) && not (isJust (requestSessionArtifacts args))
      && Map.null (requestRetainedGenerations args)
      && isNothing (preparedExactCompilation prepared)) $
    writeCompileInputProof outDir (prHscEnv (pprPipelineResult prepared))
      (preparedFreshDependencies prepared) (pprPackageImports prepared)

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
  :: YieldSiteDelivery -> FilePath -> [CoreBind] -> Maybe T.Text -> [T.Text]
  -> [PreparedArtifact] -> IO ()
writePreparedSidecars delivery outDir binds capturedType warnings artifacts = do
  let constructors = concatMap paConstructors artifacts
  metadata <- either (ioError . userError . show) pure (metadataForConstructors constructors)
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
runTurnMode :: BoundCompilerRequest -> FilePath -> IO [Diag]
runTurnMode bound path = do
  let args = boundWorkerRequest bound
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
    let parserFlags = boundParserFlags bound
    sb <- case mVerdict of
      Just verdict -> pure verdict
      Nothing -> timePhase timing "classify"
        (evaluate (classifyWithFlags parserFlags turnSrc))
    let exact = boundRequestExactScope bound
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
        (_spliced, modName, modulePath) <- writeSplicedModule parserFlags outDir lastAttempt spliced
        items <- timePhase timing "declaration_binders" $ runBoundGhc bound (extractBindersNamedGhc modulePath (requestIncludes args) modName)
        let binders = if null (sbBinders sb)
                        then map (T.pack . exportItemName) items
                        else map T.pack (sbBinders sb)
        return (TDecl binders items declarationSource)
      _kind -> do
        compileClassifiedTurn parserFlags bound timing outDir turnSrc sb bindersStr admittedItem lastAttempt exact
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
  either throwIO (const (pure [])) res

-- | Write @spliced@ (a fully-rendered module source) to a scratch file under
-- 'outDir', named for its own @module X where@ header, and retain it in
-- 'lastAttempt' for on-failure diagnostics. Shared by 'runTurnMode' (the
-- @decl@ verdict, directly) and 'compileClassifiedTurn' (every other
-- verdict, via its own @spliceInto@).
writeSplicedModule
  :: DynFlags -> FilePath -> IORef (Maybe (FilePath, String)) -> String
  -> IO (String, String, FilePath)
writeSplicedModule flags outDir lastAttempt spliced = do
  createDirectoryIfMissing True outDir
  -- A refused header still retains the exact generated source for diagnostics.
  -- Successful TurnOut already contains this source.
  writeIORef lastAttempt (Just (outDir </> "turn-attempt.hs", spliced))
  effective <- templateParserFlags flags spliced >>= either throwCellSplitError pure
  declared <- either fail pure (extractModuleName effective spliced)
  let modName = fromMaybe "Main" declared
      modulePath = outDir </> modName ++ ".hs"
  writeFile modulePath spliced
  return (spliced, modName, modulePath)

-- | Compile one non-declaration turn from authored syntax or an admitted
-- native recipe. Checked signatures remain compiler payloads, never type text.
compileClassifiedTurn
  :: DynFlags -> BoundCompilerRequest -> Bool -> FilePath
  -> String -> StmtBinders -> String -> Maybe CheckedItemAdmission
  -> IORef (Maybe (FilePath, String))
  -> Maybe ExactScope
  -> IO TurnOut
compileClassifiedTurn flags bound timing outDir turnSrc sb bindersStr admitted lastAttempt exact =
  compiledTurn <$> compileClassifiedTurnKeeping bound timing outDir turnSrc sb bindersStr admitted lastAttempt [] (StandaloneTurnParse flags) exact

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
  :: BoundCompilerRequest -> Bool -> FilePath
  -> String -> StmtBinders -> String -> Maybe CheckedItemAdmission
  -> IORef (Maybe (FilePath, String)) -> [String] -> TurnParseContext -> Maybe ExactScope -> IO CompiledTurnOutput
compileClassifiedTurnKeeping bound timing outDir turnSrc sb bindersStr admitted lastAttempt programImports parseContext exact = do
    let args = boundWorkerRequest bound
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
        -- The scratch path uses the generated header's identity; compilation
        -- selects the target's GHC summary by that source path.
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
                    renamed <- either fail pure (renameScaffoldModuleHeader selectedFlags owner originalTemplate)
                    protectedOccurrences <- either fail pure
                      (captureProtectedTemplateImports selectedFlags renamed)
                    renamedTemplate <- prepareTemplate renamed
                    (finalSource, linePrefix) <- renderRecipe renamedTemplate
                    renderedOccurrences <- either fail pure
                      (renderProtectedTemplateImports linePrefix protectedOccurrences)
                    pure (renamed, finalSource, renderedOccurrences)
          (finalProtected, finalSource, finalOccurrences) <- renderNamedRecipe
          finalOutput <- writeSplicedModule selectedFlags outDir lastAttempt finalSource
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
          compilePreparedRequest bound (PreparedProducts (requestModuleCandidates args))
            (Map.keysSet (requestRetainedGenerations args)) purpose
            (Just scope) modulePath
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
    (variant, spliced, completion) <- compileVariants (0 :: Int) matching
    let prepared = preparedCompilationResult completion
        result = pprPipelineResult prepared
        binds       = prBinds result
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
    (preparedArtifacts, productContext) <- prepareArtifacts originalInterfaces outDir completion
      [preparedScaffoldTargetName] (standardAuxiliaryRoots binds)
      (requestRetainedGenerations args) (sessionBindingRepresentations sessionBindings)
    let asksSites = concatMap paYieldSites preparedArtifacts
    timePhase timing "prepared_sidecars" $ writePreparedSidecars InlineYieldSites outDir binds mCapturedTy warnTexts preparedArtifacts
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
          bindings <- writeSessionBindings generation root sessionBindings
          return (TBind [T.pack observation] variant bindings asksSites wrapped)
        Nothing -> return (TExpr variant asksSites wrapped)
      SDecl -> error ("--turn: unexpected verdict kind: " ++ templateSelectorWireName selector)
    let owner = moduleNameString (moduleName (tcg_mod (prTargetTcGblEnv result)))
    pure (CompiledTurnOutput turn prepared originalProducts owner)

-- | Block classify mode (@--classify@):
-- Classify every input using the capability's retained parser defaults and
-- write the verdicts in argv order to
-- @--classify-out@. Serves @tidepool-repl@'s block runner, which segments a
-- block into decl runs before compiling any item and so needs every verdict
-- up front — one spawn for the whole block instead of one classify spawn per
-- item.
runClassifyMode :: DynFlags -> Bool -> WorkerRequest -> IO [Diag]
runClassifyMode parserFlags timing args =
  timePhase timing "total" $
    trySynchronous
      ( do
          out      <- requireArg "--classify-out" (requestClassifyOut args)
          srcs     <- mapM readFile (requestFiles args)
          verdicts <- timePhase timing "classify" (mapM (evaluate . classifyWithFlags parserFlags) srcs)
          writeFile out (renderVerdictsJson verdicts)
          hPutStrLn stderr $ "  Wrote: " ++ out ++ " (" ++ show (length verdicts) ++ " verdicts)"
     )
      >>= either throwIO (const (pure []))

-- | Split, classify, and typecheck one notebook cell in a single worker
-- request. Rust authors the module template (scope/import/effect-row policy);
-- GHC owns every Haskell decision and returns post-zonk statement binder pins.
-- Parsing chooses execution ordinals before runtime reserves original owners.
-- The emitted receipt contains no checked pins, prepared bodies or live values.
runCellPlanMode :: DynFlags -> WorkerRequest -> FilePath -> IO [Diag]
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
  either throwIO (const (pure [])) result

runCellMode :: BoundCompilerRequest -> FilePath -> IO [Diag]
runCellMode bound cellPath = do
  let exact = boundRequestExactScope bound
  case exact >>= \scope -> (,) scope <$> (scopeCheckedCell scope >>= checkedPlannedCell) of
    Just (scope, planned) -> runCellProgramMode bound cellPath scope planned
    Nothing -> runLegacyCellMode bound cellPath

runLegacyCellMode :: BoundCompilerRequest -> FilePath -> IO [Diag]
runLegacyCellMode bound cellPath = do
  let args = boundWorkerRequest bound
      parserFlags = boundParserFlags bound
  provisionalOutput <- newIORef Nothing
  res <- trySynchronous $ do
    cellSource <- readFile cellPath
    templatePath <- requireArg "--cell-template" (requestCellTemplate args)
    template <- readFile templatePath
    let admittedScope = boundRequestExactScope bound
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
    effectiveFlags <- templateParserFlags parserFlags initialSource >>= either throwCellSplitError pure
    declaredModule <- either fail pure (extractModuleName effectiveFlags initialSource)
    let outDir = fromMaybe
          (takeDirectory cellPath </> takeBaseName cellPath ++ "_cell")
          (requestOutDir args)
        moduleName' = fromMaybe "Main" declaredModule
        modulePath = outDir </> moduleName' ++ ".hs"
        scope = if hasSessionScope args
          then Just (scopeFromWorkerRequest args)
          else Nothing
    createDirectoryIfMissing True outDir
    out <- requireArg "--cell-out" (requestCellOut args)
    preparedDeclaration <- case admittedScope >>= \exact -> (,) exact <$> scopeCheckedCell exact of
      Just (exact, admission)
        | any ((== KDecl) . sbKind . cellAnalysisVerdict) (cellPlanItems initialPlan) ->
            Just <$> prepareOriginalCellDeclaration bound template outDir scope exact admission initialPlan
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
      compileBoundRequest bound checkedSelection Set.empty
        (withSourceImportIntents (cellPlanPrologue effective) compilePurpose)
        scope modulePath)
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
  either throwIO pure res

-- The worker owns all GHC passes of a cell. Interfaces produced here are
-- type evidence; no value or effect is evaluated by this transaction.
runCellProgramMode :: BoundCompilerRequest -> FilePath
  -> ExactScope -> PlannedCellAdmission -> IO [Diag]
runCellProgramMode bound cellPath exact planned = do
  let args = boundWorkerRequest bound
      parserFlags = boundParserFlags bound
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
  either throwIO pure attempted
  where
    compileSegment parserFlags timing admission template outDir batches state (segmentIndex, segment) = do
      let offset = programItemOffset state
          prefix = selectedProgramValues (programValues state)
          withPrefix = installProgramImports prefix (programOriginals state) segment
          scope = programExact state
          segmentRequest = cellSegmentRequest bound state
          localArgs = boundWorkerRequest segmentRequest
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
            prepareOriginalCellDeclaration segmentRequest template directory
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
                effectiveFlags <- templateParserFlags parserFlags (preparedTypedSegmentSource base)
                  >>= either throwCellSplitError pure
                named <- either fail pure (renameScaffoldModuleHeader effectiveFlags owner checkingTemplate)
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
                compiledSegment <- compilePreparedSegmentRequest segmentRequest (WithTypedSegmentPreparation prepare
                  (PreparedSegmentProducts typedPlan (requestModuleCandidates localArgs)))
                  (Map.keysSet (requestRetainedGenerations localArgs))
                  (withSourceImportIntents (cellPlanPrologue plan) purpose)
                  (Just (scopeFromWorkerRequest localArgs)) sourcePath
                pure (authored,compiledSegment)
          (finalized,(authored,compiled)) <- timePhase timing "cell_program_segment_frontend"
            (checkCellInstances compile withPrefix)
          (stagingRoot,batch) <- readIORef preparedBatch
            >>= maybe (fail "typed segment has no prepared Session batch") pure
          let (completion,typed) = compiled
              prepared = preparedCompilationResult completion
              typedPlan = preparedTypedSegmentPlan authored
              rendered = preparedTypedSegmentSource authored
              result = pprPipelineResult prepared
              items = typedSegmentItems typed
              targets = map (plannedItemEntry . typedItemPlan) items
              binds = prBinds result
              sourceOwner = moduleNameString (moduleName (tcg_mod (prTargetTcGblEnv result)))
          targetPreparedModule <- case
              [preparedModule | preparedModule <- pprModules prepared,
                pmModule preparedModule == tcg_mod (prTargetTcGblEnv result)] of
            [value] -> pure value
            values -> fail ("prepared target module selection was not unique: " ++ show (length values))
          capturesByItem <- either throwIO pure
            (typedSegmentSessionBinders (pmIdentityResolver targetPreparedModule) batch)
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
            prepareArtifacts originalInterfaces directory completion targets (standardAuxiliaryRoots binds)
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
                captures = case lookup index capturesByItem of
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
            writePreparedSidecars InlineYieldSites itemDirectory binds
              Nothing (map T.pack (prWarnings result)) [outputArtifact]
            writePreparedArtifacts itemDirectory [outputArtifact]
            writeCertifiedSegmentItemProducts prepared sharedProducts itemDirectory (paProgram outputArtifact)
            ordinal <- stagedOriginalEntryOrdinal sharedProducts (preparedRootIdentity (typedItemRoot item))
            BS.writeFile (itemDirectory </> "turn.cbor") (encodeTurnOut turn)
            writeTypedItemReceipt itemDirectory scope itemAdmission rendered typedPlan
              (preparedRootIdentity (typedSegmentOriginalRoot typed)) (preparedRootIdentity (typedItemRoot item)) ordinal
            pure (generation,captures)
          validateDependencyEvidence (preparedFreshDependencies prepared)
          when (null emitted) (fail "typed segment emitted no items")
          extended <- retainStagedProgramProducts directory prepared sharedProducts sourceOwner scope
          retainedImports <- retainProgramSourceImports (programSourceImports state) prepared
            (stagedFinalizedArtifacts sharedProducts) extended
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

programValueImport :: CompletedValueImport -> (String,[String])
programValueImport value = (completedValueModule value,map fst (completedValueBinders value))

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
  :: BoundCompilerRequest -> String -> FilePath -> Maybe SessionScope
  -> ExactScope -> CheckedCellAdmission -> CellSourcePlan
  -> IO (CellSourcePlan, PlannedDeclaration, PlannedDeclarationInventory, ExactScope,
         (PreparedPipelineResult, CertifiedOriginalProducts))
prepareOriginalCellDeclaration bound template outDir scope exact admission initial = do
  let args = boundWorkerRequest bound
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
        compilePreparedRequest bound (PreparedProducts (requestModuleCandidates args))
          (Map.keysSet (requestRetainedGenerations args))
          (withSourceImportIntents (cellPlanPrologue plan)
            (GeneratedInstanceCheck (cellGeneratedInstanceRecipe plan) OriginalDeclarationCompile)) scope sourcePath
  createDirectoryIfMissing True directory
  (finalized, completion) <- checkCellInstances compileOriginal initial
  original <- planned finalized
  let prepared = preparedCompilationResult completion
      result = pprPipelineResult prepared
      environment = prHscEnv result
      binds = prBinds result
  inventory <- certifyPlannedDeclaration original environment >>= either fail pure
  originalInterfaces <- newPreparedOriginalInterfaceArtifacts prepared directory
  (artifacts, productContext) <- prepareArtifacts originalInterfaces directory completion
    ["__result"] [] (requestRetainedGenerations args) []
  writePreparedSidecars SeparateYieldSites directory binds
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
programInterfaceRequirements :: PreparedPipelineResult -> String -> String -> IO [(String, String)]
programInterfaceRequirements prepared unit owner = do
  source <- programSourceRequirements prepared unit owner
  iface <- case Map.lookup (mkModuleName owner) (pprProductInterfaces prepared) of
    Just value | unitString (moduleUnit (mi_module value)) == unit -> pure value
    _ -> fmap fst <$> readExactInterface (prHscEnv (pprPipelineResult prepared))
      (mkModule (stringToUnit unit) (mkModuleName owner)) >>= either (fail . show) pure
  pure (nub (source ++ homeInterfaceUsageOwners (prHscEnv (pprPipelineResult prepared)) iface))

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
