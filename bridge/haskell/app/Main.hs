{-# LANGUAGE RankNTypes #-}

module Main where

import System.Environment (getArgs)
import System.FilePath (takeBaseName, takeDirectory, (</>))
import System.Directory (createDirectoryIfMissing, setCurrentDirectory)
import qualified Data.ByteString as BS
import Codec.CBOR.Encoding ( encodeBytes, encodeListLen, encodeString, encodeWord64)
import Codec.CBOR.Write (toStrictByteString)
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import Control.Exception
  ( evaluate, try, throwIO, mask, SomeException, IOException )
import Data.IORef (IORef, newIORef, readIORef, writeIORef)
import Data.List (intercalate)
import qualified Data.List.NonEmpty as NE
import Data.Maybe (fromMaybe, mapMaybe, isJust, isNothing)
import Control.Monad ( forM, forM_, when, unless, void)
import System.Exit (ExitCode(..), exitWith)
import System.IO (hPutStrLn, stderr, stdin, stdout, hSetBinaryMode, hSetEncoding, utf8, IOMode(ReadMode), withBinaryFile)

import GHC.Driver.Session (DynFlags)
import GHC.Core (Bind(..))
import qualified Data.Text as T
import qualified Data.Text.Encoding as TE

import Tidepool.Binders
  ( classifyWithFlags, exportItemName, templateParserFlags
  , analyzeOrderedCellWithFlags, PreparedTypedSegmentSource(..), CellSourcePlan(..), CellAnalysisItem(..), CellBindingForm(..), CellExpressionPlan(..), BoundBinder(..),
    SourcePrologue(..), LocatedPragma(..), ExpressionLiftPlan(..)
  , declarationSourceWithTemplateFlags, renderDeclarationForTemplate
  , TurnKind(..), parseTurnKind
  , TemplateSelector(..), templateSelectorForVerdict, templateSelectorWireName
  , StmtBinders(..), TurnOut(..), renderVerdictsJson
  )
import Tidepool.CompilerRequest
import Tidepool.GhcPipeline
  ( PipelineSelection(..), PreparedPipelineResult, pprPipelineResult, pprPackageImports, CheckedEnvironmentResult(..)
  , preparedFreshDependencies, preparedExactCompilation
  , CompilePurpose(..), prBinds, prHscEnv
  , prCapturedType, prWarnings, prWarningDiagnostics, prTargetTcGblEnv, CompilerScope
  , withResidentCompilerScopes, captureSupportedPackageProfile
  , scopedEnsureWorld, WorldCellRefusal(..)
  , satisfiesCapturedConstraint, activationPreviewInputType )
import Tidepool.CompilerProducts
  ( CertifiedOriginalProducts, PreparedProductContext
  , newPreparedOriginalInterfaceArtifacts, writeCertifiedProductsKeepingWithOriginals )
import Tidepool.ExecutionSchema (SymbolIdentity(..))
import Tidepool.CompileInput (writeCompileInputProof)
import Tidepool.ExactHydration
  ( OriginalInterfaceArtifacts, generatedActivationPreviewRecipe )
import Tidepool.DeclarationJoin
  ( DeclarationOperation(..), readDeclarationOperation, validateDeclarationJoin
  , HostBindingInterfaceInput(..), encodeHostBindingInterface, encodeBindingInterfacePurpose
  , DeclarationExport(..), ExportIdentity(..), ExportNamespace(..)
  , renderDeclarationJoinOutcome, inspectDeclarationArtifacts
  , renderDeclarationInventoryOutcome )
import qualified Tidepool.WorkerServer as WorkerServer
import Tidepool.DiagJson
  ( Diag, SourceRejection(..), InputRejection(..) )
import Tidepool.ExtractUtil (shaHex, trySynchronous)
import Tidepool.RequestCompletion
  ( Completion, PhaseSink, BodyOutcome(..), CleanupStage(CompilerScopeRestore), captureCompletion
  , flattenCompletion, completionResult, completionFailures, appendCleanupFailures, asCleanupFailures )
import Tidepool.WorkerDiagnostics
  ( throwCellSplitError, sourceFailureDiagnostics )
import Tidepool.ExtractRequest
  ( RequestOptions(..), AdmittedWorkerRequest, AdmittedRequestOperation(..)
  , SourceProductRequest, sourceProductSource, sourceProductOutput, SourceOutputKind(..), sourceOutputKind, sourceOutputDirectory, sourceOutputTargets
  , sourceInput, TurnRequest, turnSourceRequest, turnOutput, turnVerdict, turnTemplates, turnOutputDirectory
  , ClassificationRequest, classificationSources, classificationOutput
  , InspectionPayload, inspectionOutput, CellRequest, cellSourceRequest, cellTemplate, cellOutput, cellTemplates, admittedRequestOperation, admitWorkerRequest
  , workerRequestFromArgv, workerRequestFlag
  , WorldQueryRequest(..), WorldCellPlanRequest(..)
  )
import Tidepool.Introspection
  ( encodeInspectionResults, withInspectionWork
  , inspectionCompilationWork )
import Tidepool.InspectionRunner
  ( runInspectionRequests )
import Tidepool.ExactScope
  ( ExactScope, scopeRequestSha256, scopeSemanticSha256
  , ActivationPreviewAdmission(..), scopeActivationPreview, revalidateExactScope )
import Tidepool.CheckedCell (encodeCheckedSignature, captureCheckedTypeWitness, sealCheckedTypeWitness, encodeCheckedTypeWitness
  , validateOriginalInputTypeWitness)
import Tidepool.Session
  ( SessionScope(..), preparedScaffoldTargetName, SessionModule(..), SessionModuleKind(..), Generation(..)
  , sessionHiPath, sessionModuleString)
import Tidepool.SessionArtifacts
  ( prepareSessionBindings, sessionBindingRepresentations, writeSessionBindings, parseValModule
  , emitHostBindingInterface
  )
import Tidepool.CborEncode (encodeTurnOut, encodeCellOut, encodeBoundBinder)
import Tidepool.Timing (readTimingEnabled, timePhase)
import Tidepool.TurnSource
  ( extractModuleName, generatedScaffoldModuleName, renameScaffoldModuleHeader
  , captureCompilerDefaultRecipe, literalImportRendering
  , importRenderingSource, replaceTemplateMarker
  , qualifyCompilerDefaultRendering, spliceImportRendering )
import Tidepool.DependencyEvidence
  ( validateDependencyEvidence, writeDependencyEvidence)
import Tidepool.PackageWitness (requireObservationPackageEnvironment)

-- | Serve one typed request, or answer the role probe before compiler setup.
-- Source operations return one diagnostics document; source-free controls
-- return their typed result. Stderr remains the human-readable channel.
main :: IO ()
main = do
  rawWorkerRequest <- getArgs
  if rawWorkerRequest == ["--print-tidepool-role-v1"]
    then putStrLn "tidepool-extractor-role-v1 worker" >> exitWith ExitSuccess
  else if rawWorkerRequest == ["--print-worker-request-flag"]
    then putStrLn workerRequestFlag >> exitWith ExitSuccess
  else if rawWorkerRequest == ["--capture-supported-package-profile-v2"]
    then do
      hSetBinaryMode stdout True
      captureSupportedPackageProfile >>= BS.hPut stdout
  else if rawWorkerRequest == ["--worker-loop-v2"]
    then do
      hSetBinaryMode stdin True
      hSetBinaryMode stdout True
      withResidentCompilerScopes [] $ \runRequest ->
        WorkerServer.runWorkerLoop $ \serveTransaction ->
          runRequest (pure ()) $ \scope ->
            serveTransaction (\sink native cwd argv ->
              runWorkerInvocation sink native scope cwd argv)
    else do
      hSetEncoding stdout utf8
      -- Direct transport boots the same compiler owner once for all helpers.
      result <- WorkerServer.runDirectRequest (runDirectInvocation rawWorkerRequest)
      exitWith result

-- Retain the issued request result while the outer GHC scope and interpreter
-- release. A failure before issuance remains an independent operation failure.
runDirectInvocation :: [String] -> PhaseSink -> IO (Completion WorkerServer.WorkerResponse)
runDirectInvocation raw sink = case decodeWorkerInvocation raw of
  Left failure -> captureCompletion (throwIO failure)
  Right admitted -> runDirectAdmittedInvocation admitted sink

runDirectAdmittedInvocation :: AdmittedWorkerRequest -> PhaseSink -> IO (Completion WorkerServer.WorkerResponse)
runDirectAdmittedInvocation admitted sink = do
  requireObservationPackageEnvironment
  issued <- newIORef Nothing
  outer <- captureCompletion $ withResidentCompilerScopes [] $ \runRequest ->
    runRequest (pure ()) $ \scope -> mask $ \restore -> do
      completed <- restore (runAdmittedInvocation sink Nothing scope admitted)
      writeIORef issued (Just completed)
      pure completed
  known <- readIORef issued
  pure $ case known of
    Nothing -> flattenCompletion outer
    Just completed -> appendCleanupFailures completed $ case completionResult outer of
      BodySucceeded _ -> completionFailures outer
      _ -> asCleanupFailures CompilerScopeRestore outer

-- Direct and resident transports admit the same versioned request before
-- entering request-specific compiler or filesystem work.
decodeWorkerInvocation :: [String] -> Either InputRejection AdmittedWorkerRequest
decodeWorkerInvocation raw = do
  decoded <- either (Left . InvalidWorkerRequest) Right (workerRequestFromArgv raw)
  request <- maybe (Left (InvalidWorkerRequest "worker requires a versioned request")) Right decoded
  either (Left . InvalidWorkerRequest . show) Right (admitWorkerRequest request)

runWorkerInvocation :: PhaseSink -> Maybe WorkerServer.NativeDemandExchange -> CompilerScope -> FilePath -> [String] -> IO (Completion WorkerServer.WorkerResponse)
runWorkerInvocation sink native compilerScope cwd raw = case decodeWorkerInvocation raw of
  Left failure -> captureCompletion (throwIO failure)
  Right admitted -> setCurrentDirectory cwd >> runAdmittedInvocation sink native compilerScope admitted

runAdmittedInvocation :: PhaseSink -> Maybe WorkerServer.NativeDemandExchange -> CompilerScope -> AdmittedWorkerRequest -> IO (Completion WorkerServer.WorkerResponse)
runAdmittedInvocation sink native compilerScope admitted = case admittedRequestOperation admitted of
  AdmittedEnsureExactInputs request -> captureCompletion $
    WorkerServer.ExactInputsResponse <$> scopedEnsureWorld compilerScope request
  AdmittedWorldCellPrepare _ | isNothing native -> captureCompletion (throwIO WorldCellUnsupported)
  _ -> fmap WorkerServer.DiagnosticResponse <$>
    (flattenCompletion <$> captureCompletion (do
      timing <- readTimingEnabled
      withBoundCompilerRequestWithNativeDemand sink native compilerScope admitted (dispatch timing)))

-- Required inputs belong to the admitted payload; dispatch never recovers them
-- from mode flags or optional raw fields.
dispatch :: Bool -> BoundCompilerRequest -> IO [Diag]
dispatch timing bound = case boundAdmittedOperation bound of
  AdmittedEnsureExactInputs _ -> fail "source-free exact input acquisition entered source dispatch"
  AdmittedDeclaration manifest output -> runDeclarationOperation bound manifest output
  AdmittedWorldCellPrepare _ -> do
    outcome <- prepareWorldCell bound
    case outcome of
      Left _ -> pure []
      Right completed -> do
        mapM_ (commitPreparedWorldDependencies bound) completed
        pure (concatMap (prWarningDiagnostics . pprPipelineResult . preparedCompilationResult) completed)
  AdmittedWorldCellPlan question -> do
    bytes <- runBoundWorldCellPlan bound
    BS.writeFile (worldCellPlanOutput question) bytes
    pure []
  AdmittedWorldQuery question -> do
    bytes <- runBoundWorldQuery bound
    BS.writeFile (worldQueryOutput question) bytes
    pure []
  AdmittedActivationPreview request -> runActivationPreviewMode bound request
  AdmittedSourceCheck request -> runSourceCheckMode bound (sourceInput request)
  AdmittedCellPlan request -> runCellPlanMode (boundParserFlags bound) (boundWorkerRequest bound) request
  AdmittedClassification request -> runClassifyMode (boundParserFlags bound) timing request
  AdmittedInspection request -> runInspectionMode bound request
  AdmittedTurn request -> runTurnMode bound request
  AdmittedSource request -> timePhase timing "total" (processFile bound timing request)

-- Compile a pure function over an already mounted input. No value interface
-- or authored completion is issued by either the opaque probe or final pass.
runActivationPreviewMode :: BoundCompilerRequest -> TurnRequest -> IO [Diag]
runActivationPreviewMode bound request = do
  let path = sourceInput (turnSourceRequest request)
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
    let outDir = fromMaybe (takeDirectory path </> takeBaseName path ++ "_cbor") (turnOutputDirectory request)
        opaque = "(TidepoolScaffoldText.pack \"<opaque value>\\nUse the input type to select fields or apply sessionInput.\", False)"
        basePurpose = HostActivationPreviewCompile (previewInputSignature admission)
        render body = do
          source <- either fail pure (replaceTemplateMarker "{{ACTIVATION_PREVIEW}}" body original)
          let owner = generatedScaffoldModuleName
                ["activation-renderer-scaffold-v1", previewOriginalContextDigest admission, source]
          protected <- either fail pure (renameScaffoldModuleHeader selectedFlags owner original)
          named <- either fail pure (replaceTemplateMarker "{{ACTIVATION_PREVIEW}}" body protected)
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
    completion <- compilePreparedRequest bound (PreparedProducts (optionModuleCandidates args)) Set.empty purpose
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
        void $ writePreparedSidecars InlineYieldSites outDir binds
          (T.pack <$> prCapturedType compiled) (map T.pack (prWarnings compiled)) artifacts
        void $ writePreparedArtifacts outDir artifacts
        void $ writeRequestProducts bound originalInterfaces outDir prepared productContext artifacts
        let outFile = turnOutput request
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
    compiled <- compileBoundRequest bound (maybe CheckedEnvironment CheckedEnvironmentProducts (optionModuleCandidates args))
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
          scope <- maybe (fail "host interface requires a bound exact scope") pure
            (boundRequestExactScope bound)
          (binder, issuedPurpose) <- emitHostBindingInterface operations scope input
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

runInspectionMode :: BoundCompilerRequest -> InspectionPayload -> IO [Diag]
runInspectionMode bound request = do
  let args = boundWorkerRequest bound
  res <- trySynchronous $ withInspectionWork $ \work -> do
    let out = inspectionOutput request
    let scope = if hasSessionScope args then Just (scopeFromWorkerRequest args) else Nothing
        compile purpose path = do
          inspectionCompilationWork work
          compileBoundRequest bound CheckedEnvironment Set.empty purpose scope path

        inspect = inspectBoundCheckedEnvironment bound work
    bytes <- encodeInspectionResults <$> runInspectionRequests request compile inspect
    BS.writeFile out bytes
  either throwIO (const (pure [])) res


-- | Whether a generic extraction needs stable session values in scope.
hasSessionScope :: RequestOptions -> Bool
hasSessionScope args = not (null (optionInjectVals args))
  || isJust (optionSessionRoot args) || isJust (optionSessionArtifacts args)

-- | Project the session portion of a worker request. Callers decide whether
-- the resulting scope is active.
scopeFromWorkerRequest :: RequestOptions -> SessionScope
scopeFromWorkerRequest args = SessionScope
  { ssRoot      = fromMaybe "" (optionSessionRoot args)
  , ssValIfaces = mapMaybe parseValModule (optionInjectVals args)
  , ssExactScope = optionSessionArtifacts args
  , ssIncarnation = optionSessionIncarnation args
  }

processFile
  :: BoundCompilerRequest -> Bool -> SourceProductRequest -> IO [Diag]
processFile bound timing request = do
  let path = sourceInput (sourceProductSource request)
      outputRequest = sourceProductOutput request
  let args = boundWorkerRequest bound
  let mOutDir = sourceOutputDirectory outputRequest
  hPutStrLn stderr $ "Processing: " ++ path
  res <- trySynchronous $ do
    -- Multi-target extraction can inject stable session values without
    -- becoming a session bind/reference operation.
    let scope = if hasSessionScope args then Just (scopeFromWorkerRequest args) else Nothing
    let purpose = if sourceOutputKind outputRequest == CertifyHomeOriginals
          then CertifyHomeProductsCompile else GeneralCompile
    completion <- compilePreparedRequest bound (PreparedProducts (optionModuleCandidates args)) (Map.keysSet (optionRetainedGenerations args)) purpose scope path
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

    let preparedTargets = NE.toList (sourceOutputTargets outputRequest)
    originalInterfaces <- newPreparedOriginalInterfaceArtifacts prepared outDir
    output <- prepareArtifactsWithIntent
      originalInterfaces outDir completion preparedTargets
      (standardAuxiliaryRoots binds) (optionRetainedGenerations args) []
    let preparedArtifacts = case output of
          NativeOutputs artifacts _ -> artifacts
          PortableOutputs artifacts -> artifacts
    void $ if null preparedArtifacts
      then ioError (userError "prepared extraction requires --target or --targets")
      else timePhase timing "prepared_sidecars" $ writePreparedSidecars SeparateYieldSites outDir binds mCapturedTy warnTexts preparedArtifacts

    void $ timePhase timing "prepared_write" $ writePreparedArtifacts outDir preparedArtifacts
    case output of
      NativeOutputs _ productContext -> void $ timePhase timing "module_products" $
        writeRequestProducts bound originalInterfaces outDir prepared (Just productContext) preparedArtifacts
      PortableOutputs _ -> do
        _ <- writeDependencyEvidence outDir (preparedFreshDependencies prepared)
        writeRequestInputProof bound outDir prepared
        BS.writeFile (outDir </> "portable-output.mode") "TPPORTABLEOUTPUT1\n"

  either throwIO (const (pure [])) res

-- Every completed worker product carries its original input evidence. Exact
-- and session requests use their separate retained authority instead.
writeRequestProducts
  :: BoundCompilerRequest -> OriginalInterfaceArtifacts -> FilePath -> PreparedPipelineResult -> Maybe PreparedProductContext
  -> [PreparedArtifact] -> IO CertifiedOriginalProducts
writeRequestProducts bound originalInterfaces outDir prepared productContext preparedArtifacts = do
  let args = boundWorkerRequest bound
  products <- writeCertifiedProductsKeepingWithOriginals (optionIncludes args) originalInterfaces outDir prepared productContext
    [(paTarget artifact, paProgram artifact) | artifact <- preparedArtifacts]
  writeRequestInputProof bound outDir prepared
  pure products

writeRequestInputProof :: BoundCompilerRequest -> FilePath -> PreparedPipelineResult -> IO ()
writeRequestInputProof bound outDir prepared =
  let args = boundWorkerRequest bound
      sourceProofAllowed = case boundAdmittedOperation bound of
        AdmittedActivationPreview _ -> False
        _ -> True
  in when (sourceProofAllowed
      && null (optionInjectVals args) && not (isJust (optionSessionArtifacts args))
      && Map.null (optionRetainedGenerations args)
      && isNothing (preparedExactCompilation prepared)) $
    writeCompileInputProof outDir (prHscEnv (pprPipelineResult prepared))
      (preparedFreshDependencies prepared) (pprPackageImports prepared)

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
runTurnMode :: BoundCompilerRequest -> TurnRequest -> IO [Diag]
runTurnMode bound request = do
  let path = sourceInput (turnSourceRequest request)
  let args = boundWorkerRequest bound
  timing <- readTimingEnabled
  hPutStrLn stderr $ "Processing (turn): " ++ path
  lastAttempt <- newIORef Nothing
  res <- timePhase timing "total" $ trySynchronous $ do
    turnSrc   <- readFile path
    let templates = turnTemplates request
    mVerdict  <- traverse parseTurnVerdictArg (turnVerdict request)
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
    let outDir     = fromMaybe (takeDirectory path </> takeBaseName path ++ "_cbor") (turnOutputDirectory request)
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
        items <- timePhase timing "declaration_binders" $ extractBoundDeclarationBinders bound modulePath modName
        let binders = if null (sbBinders sb)
                        then map (T.pack . exportItemName) items
                        else map T.pack (sbBinders sb)
        return (TDecl binders items declarationSource)
      _kind -> do
        compileClassifiedTurn parserFlags bound timing outDir turnSrc sb bindersStr lastAttempt exact
    let outFile = turnOutput request
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

-- | Compile an ordinary non-declaration turn with its selected native template.
compileClassifiedTurn
  :: DynFlags -> BoundCompilerRequest -> Bool -> FilePath
  -> String -> StmtBinders -> String -> IORef (Maybe (FilePath, String))
  -> Maybe ExactScope -> IO TurnOut
compileClassifiedTurn parserFlags bound timing outDir turnSrc sb bindersStr lastAttempt exact = do
    let args = boundWorkerRequest bound
        semanticScopeFields = case exact of
          Just selected -> ["exact-scope-semantic", scopeSemanticSha256 selected]
          Nothing ->
            let scope = scopeFromWorkerRequest args
            in (["session-scope-root", ssRoot scope]
              ++ maybe ["session-incarnation-absent"] (\value -> ["session-incarnation", value]) (ssIncarnation scope)
              ++ concatMap (\value -> ["session-value-interface", sessionModuleString value]) (ssValIfaces scope))
        templates = boundTurnTemplates bound
        spliceInto tmplFile = do
          originalTemplate <- readFile tmplFile
          selectedFlags <- templateParserFlags parserFlags originalTemplate >>= either throwCellSplitError pure
          defaults <- either fail pure (captureCompilerDefaultRecipe selectedFlags originalTemplate)
          let renderRecipe template = do
                (prepared, _) <- either fail pure
                  (qualifyCompilerDefaultRendering defaults [] (literalImportRendering template))
                either fail pure (spliceImportRendering prepared turnSrc bindersStr)
          baseSource <- renderRecipe originalTemplate
          let includeFields = ["source-search-includes"]
                ++ concatMap (\include -> ["include-path", include]) (optionIncludes args)
              retainedFields = concat
                [ ["retained", T.unpack (symbolUnit identity), T.unpack (symbolModule identity)
                  , T.unpack (symbolNamespace identity), T.unpack (symbolOccurrence identity)]
                  ++ maybe ["no-record-parent"] (\parent -> ["record-parent", T.unpack parent])
                    (symbolRecordParent identity)
                  ++ [show generation]
                | (identity, generation) <- Map.toAscList (optionRetainedGenerations args) ]
              owner = generatedScaffoldModuleName
                (["turn-scaffold-owner-v1", "scope"] ++ semanticScopeFields
                  ++ ["template", originalTemplate, "rendered", importRenderingSource baseSource]
                  ++ includeFields ++ ["unadmitted"] ++ retainedFields)
          renamed <- either fail pure (renameScaffoldModuleHeader selectedFlags owner originalTemplate)
          finalSource <- renderRecipe renamed
          writeSplicedModule selectedFlags outDir lastAttempt (importRenderingSource finalSource)
    -- Four-shape selection (protocol note, "the verdict space has four
    -- shapes, not three"): a bind that binds no name selects its own
    -- template kind and skips the session-bind artifacts entirely —
    -- 'templateSelectorForVerdict' mirrors Rust's
    -- 'TemplateSelector::for_verdict' exactly.
    let selector = templateSelectorForVerdict (sbKind sb) (sbBinders sb)
    let scope = scopeFromWorkerRequest args
        allMatching = [f | (name, f) <- templates, name == templateSelectorWireName selector]
        matching = zip [0..] allMatching
        -- A prepared turn uses one compiler pass for the checked metadata
        -- and the prepared modules.
        compileTurn modulePath =
          compilePreparedRequest bound (PreparedProducts (optionModuleCandidates args))
            (Map.keysSet (optionRetainedGenerations args)) GeneralCompile
            (Just scope) modulePath
        compileVariants _ [] = error ("--turn: no --turn-template for kind " ++ templateSelectorWireName selector)
        compileVariants _ ((index,tmplFile):rest) = do
          (spliced, _modName, modulePath) <- spliceInto tmplFile
          attempted <- try (compileTurn modulePath)
          case attempted of
            Right prepared ->
              return (index, spliced, prepared)
            Left err@(_ :: SomeException) -> case (sourceFailureDiagnostics err, rest) of
              (Just _, _ : _) -> compileVariants (index + 1) rest
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
          _ -> []
    sessionBindings <- prepareSessionBindings boundNames result
    originalInterfaces <- newPreparedOriginalInterfaceArtifacts prepared outDir
    (preparedArtifacts, productContext) <- prepareArtifacts originalInterfaces outDir completion
      [preparedScaffoldTargetName] (standardAuxiliaryRoots binds)
      (optionRetainedGenerations args) (sessionBindingRepresentations sessionBindings)
    let asksSites = concatMap paYieldSites preparedArtifacts
    void $ timePhase timing "prepared_sidecars" $ writePreparedSidecars InlineYieldSites outDir binds mCapturedTy warnTexts preparedArtifacts
    void $ timePhase timing "prepared_write" $ writePreparedArtifacts outDir preparedArtifacts
    void $ timePhase timing "module_products" $
      writeRequestProducts bound originalInterfaces outDir prepared productContext preparedArtifacts
    -- Mutable turns never enter the artifact cache, but publication must
    -- still reject source changes observed during this compilation.
    validateDependencyEvidence (preparedFreshDependencies prepared)
    let wrapped = T.pack spliced
    turn <- case selector of
      SBind -> do
        g    <- requireArg "--bind-gen"     (optionBindGen args)
        root <- requireArg "--session-root" (optionSessionRoot args)
        bbs  <- writeSessionBindings g root sessionBindings
        return (TBind (map T.pack (sbBinders sb)) variant bbs asksSites wrapped)
      SBindDiscard -> return (TBind [] variant [] asksSites wrapped)
      SExpr -> return (TExpr variant asksSites wrapped)
      SDecl -> error ("--turn: unexpected verdict kind: " ++ templateSelectorWireName selector)
    pure turn

-- | Block classify mode (@--classify@):
-- Classify every input using the capability's retained parser defaults and
-- write the verdicts in argv order to
-- @--classify-out@. Serves @tidepool-repl@'s block runner, which segments a
-- block into decl runs before compiling any item and so needs every verdict
-- up front — one spawn for the whole block instead of one classify spawn per
-- item.
runClassifyMode :: DynFlags -> Bool -> ClassificationRequest -> IO [Diag]
runClassifyMode parserFlags timing request =
  timePhase timing "total" $
    trySynchronous
      ( do
          let out = classificationOutput request
          srcs     <- mapM readFile (classificationSources request)
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
runCellPlanMode :: DynFlags -> RequestOptions -> CellRequest -> IO [Diag]
runCellPlanMode parserFlags args request = do
  let cellPath = sourceInput (cellSourceRequest request)
  result <- trySynchronous $ do
    source <- readFile cellPath
    let templatePath = cellTemplate request
    template <- readFile templatePath
    templates <- forM (cellTemplates request) $ \(kind,path) -> do
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
          <> encodeListLen (fromIntegral (length (optionIncludes args)))
          <> foldMap text (optionIncludes args)
          <> encodeListLen (fromIntegral (length (optionInjectVals args)))
          <> foldMap text (optionInjectVals args)
          <> encodeBytes observation
          <> encodeListLen (fromIntegral (length (cellPlanItems plan)))
          <> foldMap (text . bindingForm) (cellPlanItems plan)
        bindingForm item = case cellAnalysisBindingForm item of
          Just ActionBinding -> "action"
          Just LetBinding -> "let"
          Just RecursiveBinding -> "recursive"
          Nothing -> "none"
    let out = cellOutput request
    BS.writeFile out (toStrictByteString receipt)
  either throwIO (const (pure [])) result

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
