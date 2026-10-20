{-# LANGUAGE GADTs #-}
{-# LANGUAGE RankNTypes #-}

-- | Request orchestration owns normalized admission and the executor allowance.
-- Completed preparation retains its actual recovery references and local worklist;
-- later compiles and helper operations cannot replace either.
module Tidepool.CompilerRequest
  ( BoundCompilerRequest, withBoundCompilerRequest, boundWorkerRequest
  , boundRequestOperation, boundRequestExactScope, boundParserFlags
  , compileBoundRequest, compilePreparedRequest, compilePreparedSegmentRequest
  , runBoundGhc, withBoundExactInterfaceTransaction, cellSegmentRequest
  , runBoundWorldQuery, runBoundWorldCellPlan, boundWorldCellPreparation, commitPreparedWorldDependencies
  , prepareWorldCell
  , standardAuxiliaryRoots, writePreparedArtifacts, YieldSiteDelivery(..), writePreparedSidecars
  , PreparedCompilation, preparedCompilationResult, preparedCompilationWorldInputs, prepareCompilationOriginals
  , PreparedArtifact(..), PreparedArtifactOutput(..), PreparedOutputIntent(..), preparedOutputIntent, prepareArtifacts, prepareArtifactsWithIntent
  , prepareArtifactsWithProjection, PreviewOriginalDependenciesUnavailable, requirePreviewProjection
  ) where

import Control.Exception (Exception, evaluate, throw, throwIO, fromException)
import Control.Monad (forM, forM_, unless, when)
import Data.IORef (newIORef, readIORef, writeIORef, modifyIORef')
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import Data.Maybe (isJust, isNothing, mapMaybe)
import Data.Word (Word64)
import qualified Data.ByteString as BS
import qualified Data.Text as T
import GHC.Tc.Types (tcg_mod)
import GHC.Unit.Module (moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Types (unitString)
import System.IO (hPutStrLn, stderr)
import GHC (Ghc)
import GHC.Conc (getNumCapabilities, setNumCapabilities)
import GHC.Driver.Session (DynFlags, importPaths)
import Tidepool.CellProgramState
import Tidepool.CheckedPrefixImports (CompletedValueImport)
import Tidepool.CheckedAdmission (matchesInspectionAdmission)
import Tidepool.CompilerExecution
  ( CompilerExecutionGrant, CompilerExecutor, compilerExecutionGrant
  , withCompilerExecutor, sameCompilerExecutor, requireCompilerExecutorOpen )
import Tidepool.CompilerProducts
  ( newPreparedOriginalInterfaceArtifacts, writeCertifiedSegmentProducts, writeCertifiedSegmentItemProducts
  , stagedOriginalEntryOrdinal, stagedOriginalOutputFiles, publishStagedOriginalProducts
  , certifiedOriginalProducts, certifiedFinalizedArtifacts
  , OriginalProductWorklist, observeOriginalProjectionWithRecovery, PreparedProductContext
  , prepareOriginalProductsWithCacheAndReuse, prepareOriginalProductsWithWorklist
  , prepareCompilerProjectionContext, requireOriginalExecutableGlobals, admitCurrentOriginalProducts
  , preparedCurrentOriginalInventory, preparedProductInventory, currentOriginalBinders
  , currentOriginalBindingsExcept, currentReconciledOriginalProducts )
import Tidepool.DiagJson (InputRejection(..), SourceRejection(..))
import Tidepool.ExactHydration (ExactIfaceArtifact(..), ExactInterfaceOperations, OriginalInterfaceArtifacts
  , CheckedTemplateImports(..), captureProtectedTemplateImportsAt, renderProtectedTemplateImports
  , generatedTypedSegmentRecipeWithProtectedImports, generatedCheckingTemplateRecipeWithProtectedImports)
import Tidepool.ExactScope
import Tidepool.ExecutionSchema (SymbolIdentity(..), WireProgram(..), SiteRow(..))
import qualified Tidepool.ExecutionSchema as Execution
import Tidepool.ExecutionEncode (encodeWireProgram, moduleProductInput)
import Tidepool.ExecutionProjection
  ( ProjectionContext(..), ProjectionError(..), prepareComponentProjectionWithReachability
  , projectSelectedCandidateWithHostBindings, candidateGlobals, finalizePreparedCandidate
  , preparedModuleProductConstructors, preparedModuleProductYieldSites, preparedModuleProductOutcomes
  , preparedRootIdentity )
import Tidepool.HostBindingAuthority (HostBindingRepresentation, hostBindingRepresentationJsonAuthority)
import Tidepool.ConstructorIdentity (IssuedConstructor)
import qualified Tidepool.EffectSchema
import Tidepool.CheckedCell (sealCheckedTypeWitness, CheckedSignature, captureCheckedSignature, encodeCheckedSignature)
import Tidepool.CertifiedProducts (resolvePackageGlobal)
import Tidepool.ModuleCandidates (ModuleCandidate(..), CandidateGroup(..))
import Tidepool.OriginalProductRoots (requiredOriginalPackageGlobalsWithRetained, unrecoveredExactProducts)
import Tidepool.PreparedStg (PreparedModule, pmModule, pmYieldSites, pmIdentityResolver)
import Tidepool.PreparedRecovery
  ( RecoveryFailure, RecoveredClosure(..), newPreparedRecoveryWithDemandAndReuse
  , preparedRecoveryClosure, requirePreparedRecoveryPublication )
import Tidepool.Timing (readTimingEnabled, timePhase)
import Tidepool.ExtractRequest
import Tidepool.GhcPipeline
import Tidepool.HarnessSource (spliceHarnessProfilePragma)
import Tidepool.Session
  ( SessionScope, SessionModule(..), SessionModuleKind(..), Generation(..), sessionModuleString
  , preparedResumeTargetName, preparedApplyEntryTargetName, preparedApplyValueTargetName
  , scaffoldTargetName, preparedScaffoldTargetName, CapturedSessionInterface, capturedSessionInterface, capturedSessionInterfaceEvidence )
import Tidepool.TypedSegment
  ( TypedSegment, TypedItem, TypedSegmentPlan, TypedItemPlan(..), typedItemPlan, typedItemRoot
  , typedSegmentItems, typedSegmentPlanDigest, typedItemCaptures, typedCaptureIdentifier, typedCaptureType
  , typedItemObservation, typedObservationLift, typedObservationValueType, ObservationLift(..) )
import Tidepool.RequestCompletion (Completion, CleanupStage(CapabilitiesRestore), withCompletionRelease
  , PhaseSink, CompletionPhase(..), emitPhase, releaseWithPhase)

import qualified Crypto.Hash.SHA256 as SHA256
import qualified Data.Text.Encoding as TE
import Data.Char (digitToInt, intToDigit)
import Data.List (intercalate)
import System.Directory (createDirectoryIfMissing, canonicalizePath)
import System.FilePath ((</>), makeRelative, isAbsolute, splitDirectories)
import Codec.CBOR.Encoding (Encoding, encodeBytes, encodeListLen, encodeString, encodeWord64, encodeBool)
import Codec.CBOR.Write (toStrictByteString)
import GHC.Core (Bind(..), CoreBind)
import GHC.Types.Id (idName)
import GHC.Types.Name (nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Driver.Env (hsc_dflags)
import GHC.Driver.Ppr (showSDoc)
import GHC.Utils.Outputable (ppr)
import Tidepool.Binders
  ( BoundBinder(..), CellSourcePlan(..), CellAnalysisItem(..), CellExpressionPlan(..), ExpressionLiftPlan(..)
  , PreparedTypedSegmentSource(..), prepareTypedSegmentSource, templateParserFlags, TurnOut(..), renderAskJson
  , StmtBinders(..), TurnKind(..), SourcePrologue(..), LocatedPragma(..) )
import Tidepool.CborEncode (encodeMetadata, encodeTurnOut, encodeCellOut, encodeBoundBinder, encodeTypedSegmentPlan)
import Tidepool.ExecutionEncode (encodeSymbol)
import Tidepool.Metadata (metadataForConstructors, targetBindingHasIO)
import Tidepool.TypePolicy (nominalHeadsOfType)
import Tidepool.ExtractUtil (shaHex, trySynchronous)
import Tidepool.WorkerDiagnostics (throwCellSplitError)
import Tidepool.DependencyEvidence (validateDependencyEvidence, renderDependencyQualifier)
import Tidepool.PlannedDeclaration
  ( replaceTemplateModuleHeader, originalDeclarationWrapper, preparePlannedDeclaration
  , plannedModule, plannedSource, plannedOriginalOwner, renderPlannedDeclarationInventory
  , PlannedDeclaration, PlannedDeclarationInventory, admitLocalNativeDeclaration )
import Tidepool.FinalizedModuleArtifacts
  ( finalizedLocalAdmissions, localFinalizedCore, localFinalizedInterface )
import Tidepool.TurnSource (generatedScaffoldModuleName, renameScaffoldModuleHeader)
import Tidepool.SessionArtifacts
  ( prepareTypedSegmentSessionBindings, typedSegmentSessionEnvironment, typedSegmentSessionGlobals
  , typedSegmentSessionInterfaces, typedSegmentSessionRetainedGlobals, typedSegmentSessionBindingRepresentations
  , ResolvedTypedItemBindings, resolveTypedSegmentBindings
  , resolvedTypedItemOrdinal, resolvedTypedItemBinders, resolvedTypedItemInterface )

-- No caller-maintained compiler record updates: argument normalization, exact
-- admission and the one executor are bound before any mode consumes the request.
-- The admitted operation owns every field that admission already validated;
-- later stages add only their own produced settings.
data BoundCompilerRequest = BoundCompilerRequest CompilerScope WorkerRequest
  AdmittedCompilerOperation CompilerExecutionGrant CompilerExecutor (Maybe CellCompilationInputs)

-- One operation shape after shape admission and exact/held acquisition. A
-- World preparation retains its validated immutable plan; a failed acquisition
-- is a typed refusal, not an admitted operation with absent fields.
data AdmittedCompilerOperation
  = AdmittedExactOperation (Maybe ExactScope) RequestOperation
  | AdmittedHeldQueryOperation (Maybe HeldWorldQueryBinding) WorldPublicModuleRequest
  | AdmittedHeldCellPlanOperation (Maybe HeldWorldQueryBinding) WorldCellPlanRequest
  | AdmittedHeldCellPrepareOperation HeldWorldCellPreparation

data HeldWorldCellPreparation
  = HeldWorldCellRefused WorldCellPrepareRequest WorldCellRefusal
  | HeldWorldCellAdmitted HeldWorldCellAdmission

boundWorkerRequest :: BoundCompilerRequest -> WorkerRequest
boundWorkerRequest (BoundCompilerRequest _ args _ _ _ _) = args

boundRequestOperation :: BoundCompilerRequest -> RequestOperation
boundRequestOperation (BoundCompilerRequest _ _ admitted _ _ _) = admittedTag admitted
  where
    admittedTag (AdmittedExactOperation _ operation) = operation
    admittedTag (AdmittedHeldQueryOperation _ question) = WorldPublicModuleOperation question
    admittedTag (AdmittedHeldCellPlanOperation _ question) = WorldCellPlanOperation question
    admittedTag (AdmittedHeldCellPrepareOperation preparation) =
      WorldCellPrepareOperation (heldPreparationQuestion preparation)
    heldPreparationQuestion (HeldWorldCellRefused question _) = question
    heldPreparationQuestion (HeldWorldCellAdmitted admission) = heldWorldCellQuestion admission

boundRequestExactScope :: BoundCompilerRequest -> Maybe ExactScope
boundRequestExactScope (BoundCompilerRequest _ _ admitted _ _ _) = case admitted of
  AdmittedExactOperation exact _ -> exact
  _ -> Nothing

boundParserFlags :: BoundCompilerRequest -> DynFlags
boundParserFlags (BoundCompilerRequest scope _ _ _ _ _) = scopedParserFlags scope

withBoundCompilerRequest :: PhaseSink -> CompilerScope -> WorkerRequest -> (BoundCompilerRequest -> IO a) -> IO (Completion a)
withBoundCompilerRequest sink compilerScope parsed use = do
  when (isJust (requestEnsureExactInputs parsed)) $
    fail "source-free exact input acquisition cannot enter compiler request binding"
  grant <- either fail pure (compilerExecutionGrant (requestCompilerJobs parsed))
  withCompletionRelease CapabilitiesRestore getNumCapabilities (releaseWithPhase sink CapabilitiesRestore . setNumCapabilities) $ \_previous -> do
    setNumCapabilities (requestCompilerCapabilities parsed)
    withCompilerExecutor sink grant $ \executor -> do
      args <- if requestHarnessProfile parsed && not (requestCheckSource parsed)
        then spliceHarnessProfilePragma parsed else pure parsed
      operation <- case admitRequestOperation args of
        Left InvalidSourceCheckShape -> fail "source checking cannot carry product or notebook authority"
        Left InvalidCellPlanShape -> throwIO InvalidCellPlanRequest
        Left failure -> fail (show failure)
        Right operation -> pure operation
      exact <- forM (requestSessionArtifacts args) $ \manifest -> do
        scope <- scopedReadExactScope compilerScope manifest >>= either fail pure
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
      admitted <- case operation of
        WorldPublicModuleOperation question -> AdmittedHeldQueryOperation <$>
          scopedReadHeldWorld compilerScope (worldQueryIdentity question) (worldQueryRealization question)
          <*> pure question
        WorldCellPlanOperation question -> AdmittedHeldCellPlanOperation <$>
          scopedReadHeldWorld compilerScope (worldCellPlanIdentity question) (worldCellPlanRealization question)
          <*> pure question
        WorldCellPrepareOperation question -> do
          validation <- scopedValidateWorldCell compilerScope grant executor question
          let preparation = case validation of
                Left refusal -> HeldWorldCellRefused question refusal
                Right admission -> HeldWorldCellAdmitted admission
          pure (AdmittedHeldCellPrepareOperation preparation)
        _ -> pure (AdmittedExactOperation exact operation)
      emitPhase sink BodyStarted
      result <- use (BoundCompilerRequest compilerScope args admitted grant executor Nothing)
      emitPhase sink BodyFinished
      pure result

-- A cell's issued lexical prefix changes only these compilation inputs. The
-- request's grant, physical search inputs and auxiliary owner remain bound.
data CellCompilationInputs = CellCompilationInputs (Maybe ProgramSourceImports)
  [((String,String),String)] [CompletedValueImport]

cellSegmentRequest :: BoundCompilerRequest -> ProgramCellState ExactScope -> BoundCompilerRequest
cellSegmentRequest (BoundCompilerRequest _ _ (AdmittedHeldQueryOperation _ _) _ _ _) _ = throw CheckedPurposeMismatch
cellSegmentRequest (BoundCompilerRequest _ _ (AdmittedHeldCellPlanOperation _ _) _ _ _) _ = throw CheckedPurposeMismatch
cellSegmentRequest (BoundCompilerRequest _ _ (AdmittedHeldCellPrepareOperation _) _ _ _) _ = throw CheckedPurposeMismatch
cellSegmentRequest (BoundCompilerRequest scope args (AdmittedExactOperation _ operation) grant executor _) state =
  let exact = programInput state
      values = map exactModule (maybe [] checkedValueInterfaces (scopeCheckedCell exact))
      inputs = CellCompilationInputs (programSourceImports state) (programOriginals state)
        (selectedProgramValues (programValues state))
  in BoundCompilerRequest scope (args {requestInjectVals=values,requestRetainedGenerations=programRetained state})
    (AdmittedExactOperation (Just exact) operation) grant executor (Just inputs)

cellCompilationPurpose :: CellCompilationInputs -> CompilePurpose -> CompilePurpose
cellCompilationPurpose (CellCompilationInputs imports originals values) = withProgramSourceImports imports . replacePurpose
  where
    replacePurpose purpose = case purpose of
      ParsedImportSelection intents inner -> ParsedImportSelection intents (replacePurpose inner)
      GeneratedScaffoldCompile recipe inner -> GeneratedScaffoldCompile recipe (replacePurpose inner)
      GeneratedInstanceCheck recipe inner -> GeneratedInstanceCheck recipe (replacePurpose inner)
      TypedSegmentCompile plan operations inner -> TypedSegmentCompile plan operations (replacePurpose inner)
      OriginalDeclarationCompile -> ProgramItemCompile True [] originals values
      CheckedItemCompile annotations _ selected -> ProgramItemCompile False annotations originals selected
      other -> other

compileBoundCompleted :: BoundCompilerRequest -> PipelineSelection result
  -> Set.Set SymbolIdentity -> CompilePurpose -> Maybe SessionScope -> FilePath
  -> IO (CompletedCompilerResult result)
compileBoundCompleted (BoundCompilerRequest scope args admitted grant executor cell) selection retained purpose session path = do
  requireCompilerExecutorOpen executor
  let effectivePurpose = maybe purpose (`cellCompilationPurpose` purpose) cell
  normalizedPurpose <- either throwIO pure (normalizeCompilePurpose effectivePurpose)
  case admitted of
    AdmittedExactOperation exact _ -> scopedCompileCompleted scope (WithCompilerExecution grant executor selection) retained
      (maybe normalizedPurpose (ExactScopeCompile normalizedPurpose) exact) session path
      (requestIncludes args) (requestBuildProductsDir args)
    AdmittedHeldQueryOperation _ _ -> throwIO CheckedPurposeMismatch
    AdmittedHeldCellPlanOperation _ _ -> throwIO CheckedPurposeMismatch
    AdmittedHeldCellPrepareOperation preparation -> case preparation of
      HeldWorldCellRefused _ refusal -> throwIO refusal
      HeldWorldCellAdmitted admission -> scopedCompileWorldCell scope admission
        selection retained normalizedPurpose path

compileBoundRequest :: BoundCompilerRequest -> PipelineSelection result
  -> Set.Set SymbolIdentity -> CompilePurpose -> Maybe SessionScope -> FilePath -> IO result
compileBoundRequest request selection retained purpose session path =
  completedCompilerResult <$> compileBoundCompleted request selection retained purpose session path

-- The constructor stays private. Retention does not grant executor use after its
-- bracket closes; projection checks operation entry, while submissions fence a
-- concurrent close. GHC's retained mutable package cells keep their existing owner.
-- | One completion route for a prepared compilation. The World route always
-- carries its issued input basis together with the single pending dependency
-- slot consumed by 'commitPreparedWorldDependencies'; there is no empty World
-- completion and no independent pending state.
data PreparedCompletion
  = PreparedOrdinaryCompletion
  | PreparedWorldCompletion WorldCompilerCompletion

data PreparedCompilation = PreparedCompilation PreparedPipelineResult CompilerRecoveryCaches
  CompilerExecutor (IO ()) (Maybe OriginalProductWorklist) PreparedCompletion

preparedCompilationResult :: PreparedCompilation -> PreparedPipelineResult
preparedCompilationResult (PreparedCompilation result _ _ _ _ _) = result

preparedCompilationWorldInputs :: PreparedCompilation -> Maybe WorldCellInputs
preparedCompilationWorldInputs (PreparedCompilation _ _ _ _ _ PreparedOrdinaryCompletion) = Nothing
preparedCompilationWorldInputs (PreparedCompilation _ _ _ _ _ (PreparedWorldCompletion world)) =
  Just (worldCompilerCompletionInputs world)

preparedCompilationPlannedDeclaration :: PreparedCompilation -> Maybe (PlannedDeclaration,PlannedDeclarationInventory)
preparedCompilationPlannedDeclaration (PreparedCompilation _ _ _ _ _ PreparedOrdinaryCompletion) = Nothing
preparedCompilationPlannedDeclaration (PreparedCompilation _ _ _ _ _ (PreparedWorldCompletion world)) =
  worldCompilerCompletionPlannedDeclaration world

withPreparedCompilation :: PreparedCompilation
  -> (PreparedPipelineResult -> CompilerRecoveryCaches -> CompilerExecutor -> Maybe OriginalProductWorklist -> IO a)
  -> IO a
withPreparedCompilation (PreparedCompilation result recovery executor guard worklist _) use = do
  guard
  requireCompilerExecutorOpen executor
  use result recovery executor worklist

-- Projection inputs may legitimately change entry/generation/host authority.
-- The actual compiler environment, cache references and worklist cannot change.
prepareCompilationOriginals :: PreparedCompilation -> ProjectionContext -> Set.Set SymbolIdentity
  -> IO ([PreparedModule],PreparedProductContext)
prepareCompilationOriginals completion context external =
  withPreparedCompilation completion $ \prepared recovery executor worklist ->
    prepareOriginals prepared recovery executor worklist context external

prepareOriginals :: PreparedPipelineResult -> CompilerRecoveryCaches -> CompilerExecutor
  -> Maybe OriginalProductWorklist -> ProjectionContext -> Set.Set SymbolIdentity
  -> IO ([PreparedModule],PreparedProductContext)
prepareOriginals prepared recovery executor worklist context external =
  let env = prHscEnv (pprPipelineResult prepared)
      exact = compilationScope <$> preparedExactCompilation prepared
      interfaces = pprProductInterfaces prepared
      modules = pprModules prepared
  in case worklist of
    Just completed -> prepareOriginalProductsWithWorklist completed env exact interfaces context external modules
    Nothing -> prepareOriginalProductsWithCacheAndReuse (Just (pprReuseContext prepared))
      (compilerPreparedBodies recovery) (Just (compilerOriginalProjections recovery)) executor
      env exact interfaces context external modules

compileWithWorklist :: BoundCompilerRequest -> PipelineSelection result
  -> Set.Set SymbolIdentity -> CompilePurpose -> Maybe SessionScope -> FilePath
  -> IO (CompletedCompilerResult result, Maybe OriginalProductWorklist)
compileWithWorklist request@(BoundCompilerRequest _ args _ _ executor _) selection retained purpose session path = do
  localWorklist <- newIORef Nothing
  let completion env interfaces owner exact inputs = do
        recovery <- maybe (fail "resident completion has no selected recovery context") pure (completionRecovery inputs)
        (observer,worklist) <- observeOriginalProjectionWithRecovery
          (compilerOriginalProjections recovery) (compilerPreparedBodies recovery) executor
          (requestRetainedGenerations args)
          [preparedResumeTargetName,preparedApplyEntryTargetName,preparedApplyValueTargetName] Nothing
          env interfaces owner exact inputs
        writeIORef localWorklist (Just worklist)
        pure observer
  completed <- compileBoundCompleted request (WithPreparedModuleCompletion completion selection)
    retained purpose session path
  worklist <- readIORef localWorklist
  pure (completed,worklist)

compilePreparedRequest :: BoundCompilerRequest -> PipelineSelection PreparedPipelineResult
  -> Set.Set SymbolIdentity -> CompilePurpose -> Maybe SessionScope -> FilePath -> IO PreparedCompilation
compilePreparedRequest request@(BoundCompilerRequest _ args admitted _ executor _) selection retained purpose session path = do
  (completed,worklist) <- case (admitted,preparedOutputIntent args) of
    -- World generations come from its actual completed input basis. The
    -- ordinary early observer instead reads finite request arguments; World
    -- projection uses the same captured-recovery fallback as typed segments.
    (AdmittedHeldCellPrepareOperation _,_) -> do
      completed <- compileBoundCompleted request selection retained purpose session path
      pure (completed,Nothing)
    (_,NativeOriginals) -> compileWithWorklist request selection retained purpose session path
    (_,PortableExecutable) -> do
      completed <- compileBoundCompleted request selection retained purpose session path
      pure (completed,Nothing)
  route <- preparedCompletionFor completed
  pure (PreparedCompilation (completedCompilerResult completed) (completedCompilerRecovery completed) executor (completedCompilerGuard completed) worklist route)

compilePreparedSegmentRequest :: BoundCompilerRequest -> PipelineSelection PreparedSegmentProductsResult
  -> Set.Set SymbolIdentity -> CompilePurpose -> Maybe SessionScope -> FilePath -> IO (PreparedCompilation,TypedSegment)
compilePreparedSegmentRequest request@(BoundCompilerRequest _ _ _ _ executor _) selection retained purpose session path = do
  -- Typed segment preparation acquires its own Session bindings during the
  -- compiler pass. It has no early original observer; projection uses the
  -- captured recovery context and the existing fallback path.
  completed <- compileBoundCompleted request selection retained purpose session path
  let result = completedCompilerResult completed
  route <- preparedCompletionFor completed
  pure (PreparedCompilation (preparedSegmentProducts result) (completedCompilerRecovery completed) executor (completedCompilerGuard completed) Nothing route,
    preparedSegmentCaptures result)

runBoundGhc :: BoundCompilerRequest -> Ghc a -> IO a
runBoundGhc (BoundCompilerRequest scope _ admitted _ executor _) operation = do
  requireCompilerExecutorOpen executor
  case admitted of AdmittedExactOperation _ _ -> pure (); _ -> throwIO CheckedPurposeMismatch
  scopedRunGhc scope operation

boundWorldCellPreparation :: BoundCompilerRequest -> IO (Either WorldCellRefusal HeldWorldCellAdmission)
boundWorldCellPreparation (BoundCompilerRequest _ _ admitted _ executor _) = do
  requireCompilerExecutorOpen executor
  case admitted of
    AdmittedHeldCellPrepareOperation (HeldWorldCellRefused _ refusal) -> pure (Left refusal)
    AdmittedHeldCellPrepareOperation (HeldWorldCellAdmitted admission) -> pure (Right admission)
    _ -> throwIO CheckedPurposeMismatch

-- Terminal output success reborrows the actual held owner. Pending completion
-- facts carry no source tree or usable compiler capability beyond scope END.
commitPreparedWorldDependencies :: BoundCompilerRequest -> PreparedCompilation -> IO ()
commitPreparedWorldDependencies (BoundCompilerRequest scope _ admitted _ executor _)
    (PreparedCompilation _ _ issuing guard _ route) = do
  guard
  requireCompilerExecutorOpen issuing
  requireCompilerExecutorOpen executor
  unless (sameCompilerExecutor issuing executor) (throwIO CheckedPurposeMismatch)
  case admitted of
    AdmittedHeldCellPrepareOperation preparation -> case preparation of
      HeldWorldCellRefused _ refusal -> throwIO refusal
      HeldWorldCellAdmitted admission -> do
      -- The scope's pending owner validates preflight and consumes before any
      -- mutation. Request orchestration cannot reissue or restore that state.
        case route of
          PreparedOrdinaryCompletion -> throwIO CheckedPurposeMismatch
          PreparedWorldCompletion world -> scopedCommitWorldDependencies scope admission (worldCompilerCompletionDependencies world)
    _ -> throwIO CheckedPurposeMismatch

preparedCompletionFor :: CompletedCompilerResult result -> IO PreparedCompletion
preparedCompletionFor completed = case completedCompilerWorldCompletion completed of
  Nothing -> pure PreparedOrdinaryCompletion
  Just world -> pure (PreparedWorldCompletion world)

runBoundWorldCellPlan :: BoundCompilerRequest -> IO BS.ByteString
runBoundWorldCellPlan (BoundCompilerRequest scope _ admitted grant executor _) = do
  requireCompilerExecutorOpen executor
  case admitted of
    AdmittedHeldCellPlanOperation held question -> scopedPlanWorldCell scope held grant executor question
    _ -> throwIO CheckedPurposeMismatch

runBoundWorldQuery :: BoundCompilerRequest -> IO BS.ByteString
runBoundWorldQuery (BoundCompilerRequest scope _ (AdmittedHeldQueryOperation world question) grant executor _) = do
  requireCompilerExecutorOpen executor
  scopedQueryWorld scope world grant executor question
runBoundWorldQuery _ = throwIO CheckedPurposeMismatch

withBoundExactInterfaceTransaction :: BoundCompilerRequest -> (ExactInterfaceOperations -> IO a) -> IO a
withBoundExactInterfaceTransaction (BoundCompilerRequest scope args admitted _ executor _) use = do
  requireCompilerExecutorOpen executor
  case admitted of AdmittedExactOperation _ _ -> pure (); _ -> throwIO CheckedPurposeMismatch
  withScopedExactInterfaceTransaction scope (requestIncludes args) use

data PreparedArtifact = PreparedArtifact
  { paTarget :: String
  , paProgram :: WireProgram
  , paBytes :: BS.ByteString
  , paConstructors :: [IssuedConstructor]
  , paYieldSites :: [Tidepool.EffectSchema.YieldSite]
  }

-- Project before writing artifacts so the shared constructor
-- table includes exactly the GHC constructors admitted by prepared execution.
prepareArtifacts :: OriginalInterfaceArtifacts -> FilePath -> PreparedCompilation
  -> [String] -> [String] -> Map.Map SymbolIdentity Word64 -> [HostBindingRepresentation]
  -> IO ([PreparedArtifact], Maybe PreparedProductContext)
prepareArtifacts = prepareArtifactsWithProjectionIntent NativeOriginals requireProjection

data PreparedOutputIntent = NativeOriginals | PortableExecutable deriving (Eq)

data PreparedArtifactOutput
  = NativeOutputs [PreparedArtifact] (Maybe PreparedProductContext)
  | PortableOutputs [PreparedArtifact]

-- The request chooses its output contract before module completion observers
-- run. Portable compilation keeps the same checked pipeline and executor but
-- does not observe unrelated modules as native-original export products.
preparedOutputIntent :: WorkerRequest -> PreparedOutputIntent
preparedOutputIntent args
  | requestPortableOutput args = PortableExecutable
  | otherwise = NativeOriginals

prepareArtifactsWithIntent :: PreparedOutputIntent -> OriginalInterfaceArtifacts -> FilePath -> PreparedCompilation
  -> [String] -> [String] -> Map.Map SymbolIdentity Word64 -> [HostBindingRepresentation]
  -> IO PreparedArtifactOutput
prepareArtifactsWithIntent intent interfaces directory completion targets auxiliary retained hostBindings = do
  (artifacts,products) <- prepareArtifactsWithProjectionIntent intent requireProjection
    interfaces directory completion targets auxiliary retained hostBindings
  pure $ case intent of
    NativeOriginals -> NativeOutputs artifacts products
    PortableExecutable -> PortableOutputs artifacts

prepareArtifactsWithProjection
  :: (forall a. Either ProjectionError a -> IO a)
  -> OriginalInterfaceArtifacts -> FilePath -> PreparedCompilation
  -> [String] -> [String] -> Map.Map SymbolIdentity Word64 -> [HostBindingRepresentation]
  -> IO ([PreparedArtifact], Maybe PreparedProductContext)
prepareArtifactsWithProjection = prepareArtifactsWithProjectionIntent NativeOriginals

prepareArtifactsWithProjectionIntent
  :: PreparedOutputIntent -> (forall a. Either ProjectionError a -> IO a)
  -> OriginalInterfaceArtifacts -> FilePath -> PreparedCompilation
  -> [String] -> [String] -> Map.Map SymbolIdentity Word64 -> [HostBindingRepresentation]
  -> IO ([PreparedArtifact], Maybe PreparedProductContext)
prepareArtifactsWithProjectionIntent _ _ _ _ completion [] _ _ _ =
  withPreparedCompilation completion (\_ _ _ _ -> pure ([], Nothing))
prepareArtifactsWithProjectionIntent intent project originalInterfaces outDir completion targets@(firstTarget : _) auxiliaryRoots retainedGenerations hostBindings =
  withPreparedCompilation completion $ \prepared recoveryCaches executor _ -> do
    timing <- readTimingEnabled
    let result = pprPipelineResult prepared
        hscEnv = prHscEnv result
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
          ([binder | exactProduct <- exactProducts, group <- originalGroups exactProduct
            , binder <- originalBinders group]
           ++ [binder | candidate <- candidates, group <- candidateGroups candidate
            , binder <- candidateGroupBinders group])
    (originalModules,productContext,originalBindings,admittedOriginalBinders,
      originalConstructors,originalSites,originalPackageGlobals) <- case intent of
      PortableExecutable -> do
        unless (isNothing exactScope && null candidates && Map.null retainedGenerations
            && null hostBindings) $
          fail "portable output cannot consume retained native or host-binding inputs"
        pure (modules,Nothing,Map.empty,Set.empty,[],[],const (Right []))
      NativeOriginals -> do
        (originalModules,rawProductContext) <- timePhase timing "prepared_original_demand" $
          prepareCompilationOriginals completion (contextFor firstTarget) externalOriginalBinders
        productContext <- admitCurrentOriginalProducts originalInterfaces outDir prepared rawProductContext
        inventory <- maybe (fail "current original admission did not issue its inventory") pure
          (preparedCurrentOriginalInventory productContext)
        originalConstructors <- project (preparedModuleProductConstructors (preparedProductInventory productContext))
        originalSites <- project (preparedModuleProductYieldSites (preparedProductInventory productContext))
        let products = preparedProductInventory productContext
            outputRoots = Set.fromList (map (projectionEntry . contextFor) targets
              ++ projectionAuxiliaryRoots firstContext)
            originalBindings = currentOriginalBindingsExcept inventory outputRoots
            admittedOriginalBinders = Set.union externalOriginalBinders (currentOriginalBinders inventory)
            originalProducts =
              [(unitString (moduleUnit owner), moduleNameString (moduleName owner),
                either (Left . show) Right outcome)
              | (owner, outcome) <- preparedModuleProductOutcomes products]
            exactOriginals =
              [(originalUnit originalProduct, originalModule originalProduct,
                [(originalOrdinal group, originalBinders group, originalGlobals group)
                 | group <- originalGroups originalProduct])
              | originalProduct <- unrecoveredExactProducts (currentReconciledOriginalProducts inventory)]
            originalPackageGlobals = requiredOriginalPackageGlobalsWithRetained
              originalProducts candidates exactOriginals (Map.keysSet retainedGenerations)
        pure (originalModules,Just productContext,originalBindings,admittedOriginalBinders,
          originalConstructors,originalSites,originalPackageGlobals)
    let withOriginals context = context {projectionCurrentOriginals = originalBindings}
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
    recover <- newPreparedRecoveryWithDemandAndReuse (Just (pprReuseContext prepared)) (Just executor) demandPackages
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
      when (intent == PortableExecutable) $ do
        let homeUnit = T.pack (unitString (moduleUnit targetModule))
            unresolvedHome = [Execution.globalIdentity global | global <- programGlobals program
              , symbolUnit (Execution.globalIdentity global) == homeUnit]
        unless (null unresolvedHome) $
          fail ("portable output has unresolved home imports: " ++ show unresolvedHome)
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
    pure (artifacts, productContext)

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

reportRecoveryResiduals :: String -> [RecoveryFailure] -> IO ()
reportRecoveryResiduals _ [] = pure ()
reportRecoveryResiduals target failures =
  hPutStrLn stderr $ "  Prepared recovery residuals (" ++ target ++ "): "
    ++ show failures

renderAsksJson :: [Tidepool.EffectSchema.YieldSite] -> String
renderAsksJson sites = "[" ++ intercalate "," (map renderAskJson sites) ++ "]"

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

writePreparedArtifacts :: FilePath -> [PreparedArtifact] -> IO [FilePath]
writePreparedArtifacts outDir artifacts = forM artifacts $ \artifact -> do
  let name = paTarget artifact ++ ".prepared.cbor"
      output = outDir </> name
  BS.writeFile output (paBytes artifact)
  hPutStrLn stderr $ "  Wrote: " ++ output ++ " (prepared execution)"
  pure name

data YieldSiteDelivery = SeparateYieldSites | InlineYieldSites

writePreparedSidecars
  :: YieldSiteDelivery -> FilePath -> [CoreBind] -> Maybe T.Text -> [T.Text]
  -> [PreparedArtifact] -> IO [FilePath]
writePreparedSidecars delivery outDir binds capturedType warnings artifacts = do
  let constructors = concatMap paConstructors artifacts
  metadata <- either (ioError . userError . show) pure (metadataForConstructors constructors)
  let hasIO = any (targetBindingHasIO binds . paTarget) artifacts
      metaBytes = encodeMetadata metadata hasIO capturedType warnings
  BS.writeFile (outDir </> "meta.cbor") metaBytes
  auxiliary <- case delivery of
    InlineYieldSites -> pure [] -- TurnOut carries the same typed sites.
    SeparateYieldSites -> do
      let multiple = length artifacts > 1
      forM artifacts $ \artifact -> do
        let asksName = if multiple then paTarget artifact ++ ".asks.json" else "asks.json"
        writeFile (outDir </> asksName) (renderAsksJson (paYieldSites artifact))
        pure asksName
  pure ("meta.cbor":auxiliary)

-- This initial slice performs actual whole-segment inference/output emission
-- from captured current canonical source imports and exact checked-value
-- aliases whose types need only fixed packages. Other closures are refused.
-- It never
-- constructs an ExactScope or publishes a runtime world during compilation.
-- Per-item facts admitted for one held-world cell. Parsed source, captures,
-- signatures, issued interface and artifact are correlated by exact identity
-- before any terminal manifest or response is exposed.
data WorldCellItemSource = WorldCellItemSource
  { worldCellItemTyped :: TypedItem
  , worldCellItemParsed :: CellAnalysisItem
  , worldCellItemBindings :: ResolvedTypedItemBindings
  , worldCellItemSignatures :: [CheckedSignature]
  , worldCellItemArtifact :: PreparedArtifact
  }

worldCellItemCaptures :: WorldCellItemSource -> [BoundBinder]
worldCellItemCaptures = resolvedTypedItemBinders . worldCellItemBindings

worldCellItemInterface :: WorldCellItemSource -> Maybe CapturedSessionInterface
worldCellItemInterface = resolvedTypedItemInterface . worldCellItemBindings

data PreparedWorldCellItem = PreparedWorldCellItem
  { preparedWorldCellItemManifest :: (Word64,Word64,BS.ByteString)
  , preparedWorldCellObservation :: Maybe CellExpressionPlan
  }

-- These outputs remain private until every admitted stage has completed. The
-- admission owns the compiler cursor; this record owns only emitted DATA.
data PreparedWorldCellStage = PreparedWorldCellStage
  { preparedWorldStageItems :: [PreparedWorldCellItem]
  , preparedWorldStagePlan :: CellSourcePlan
  , preparedWorldStageSource :: String
  , preparedWorldStageInputs :: [WorldCellInputReference]
  , preparedWorldStageFiles :: [(FilePath,BS.ByteString)]
  , preparedWorldStageTypedPlans :: [TypedSegmentPlan]
  , preparedWorldStageReceipt :: Encoding
  , preparedWorldStageCompletion :: PreparedCompilation
  }

correlateWorldCellItems
  :: [TypedItem] -> [PreparedArtifact] -> [(Int,CellAnalysisItem)]
  -> [ResolvedTypedItemBindings] -> [(Int,[CheckedSignature])] -> IO [WorldCellItemSource]
correlateWorldCellItems typedItems artifacts parsedItems captures signatures = do
  let itemPlan item = typedItemPlan item
      itemOrdinal = plannedItemOrdinal . itemPlan
      itemTarget = plannedItemEntry . itemPlan
      expected = Set.fromList (map itemOrdinal typedItems)
      targets = Set.fromList (map itemTarget typedItems)
  unless (Set.size expected == length typedItems)
    (fail "world typed items repeat an item ordinal")
  unless (Set.size targets == length typedItems)
    (fail "world typed items repeat an item target")
  capturesByItem <- uniqueEntries "world item captures" resolvedTypedItemOrdinal captures
  signaturesByItem <- Map.map snd <$> uniqueEntries "world item capture signatures" fst signatures
  unless (Map.keysSet capturesByItem == expected)
    (fail "world item captures do not cover exactly the typed items")
  unless (Map.keysSet signaturesByItem == expected)
    (fail "world item capture signatures do not cover exactly the typed items")
  artifactsByTarget <- uniqueEntries "world item artifacts" paTarget artifacts
  unless (Map.keysSet artifactsByTarget == targets)
    (fail "world item artifacts do not cover exactly the typed items")
  parsedByKey <- Map.map snd <$> uniqueEntries "world parsed execution items" fst parsedItems
  unless (Map.keysSet parsedByKey == expected)
    (fail "world parsed items do not cover exactly the typed items")
  sources <- forM typedItems $ \item -> do
    let plan = itemPlan item
        key = plannedItemOrdinal plan
    parsed <- maybe (fail "world typed item lost its parsed source") pure
      (Map.lookup key parsedByKey)
    selected <- maybe (fail "world typed item lost its captures") pure
      (Map.lookup key capturesByItem)
    itemSignatures <- maybe (fail "world typed item lost its capture witnesses") pure
      (Map.lookup key signaturesByItem)
    artifact <- maybe (fail "world typed item lost its prepared artifact") pure
      (Map.lookup (plannedItemEntry plan) artifactsByTarget)
    pure (WorldCellItemSource item parsed selected itemSignatures artifact)
  pure sources
  where
    uniqueEntries label key values = do
      let keyed = Map.fromList [(key value,value) | value <- values]
      unless (length values == Map.size keyed)
        (fail (label ++ " repeat an item identity"))
      pure keyed

prepareWorldCell :: BoundCompilerRequest -> IO (Either WorldCellRefusal [PreparedCompilation])
prepareWorldCell bound = case boundRequestOperation bound of
  WorldCellPrepareOperation question -> prepareWorldCellQuestion bound question
  _ -> throwIO CheckedPurposeMismatch

prepareWorldCellQuestion :: BoundCompilerRequest -> WorldCellPrepareRequest -> IO (Either WorldCellRefusal [PreparedCompilation])
prepareWorldCellQuestion bound question = do
  admitted <- boundWorldCellPreparation bound
  case admitted of
    Left refusal -> rejected refusal
    Right preparation
      | let initial = heldWorldCellPlan preparation
      , null (cellPlanItems initial) -> writeResult (Right []) (encodeCellOut initial [] [] "") [] [] [] [] >> pure (Right [])
      | otherwise -> do
          let initial = heldWorldCellPlan preparation
          attempted <- trySynchronous (prepareStages preparation [])
          case attempted of
            Right stages -> do
              let items = concatMap preparedWorldStageItems stages
                  finalized = initial {cellPlanItems = concatMap (cellPlanItems . preparedWorldStagePlan) stages}
                  observations = encodeCellOut finalized [] (mapMaybe preparedWorldCellObservation items)
                    (concatMap preparedWorldStageSource stages)
                  inputs = Set.toAscList (Set.fromList (concatMap preparedWorldStageInputs stages))
                  common = concatMap preparedWorldStageFiles stages
                  plans = concatMap preparedWorldStageTypedPlans stages
              unless (map (\item -> let (ordinal,_,_) = preparedWorldCellItemManifest item in ordinal) items
                    == map fst (heldWorldCellSlots preparation)
                  && Map.size (Map.fromList common) == length common)
                (throwIO WorldCellCorrupt)
              writeResult (Right items) observations inputs common plans (map preparedWorldStageReceipt stages)
              pure (Right (map preparedWorldStageCompletion stages))
            Left failure -> case fromException failure of
              Just refusal -> rejected refusal
              Nothing -> case fromException failure of
                Just policy@(WorldModulePolicyFailure _ _) -> do
                  emitWorldAcquisitionRejection "world cell policy" (worldCellPrepareOutput question) (show policy)
                  rejected WorldCellUnsupported
                Nothing -> throwIO failure
  where
    rejected refusal = writeResult (Left refusal) BS.empty [] [] [] [] >> pure (Left refusal)
    prepareStages preparation reversed = do
      next <- case bound of
        BoundCompilerRequest scope _ _ _ _ _ -> scopedPrepareWorldCellStage scope preparation
      case next of
        Nothing -> pure (reverse reversed)
        Just stage -> do
          let initial = heldWorldCellStagePlan stage
              slots = heldWorldCellStageSlots stage
              offset = heldWorldCellStageOffset stage
              stageDirectory = directory </> "stage-" ++ show offset
          output <- case cellPlanItems initial of
            [item] | sbKind (cellAnalysisVerdict item) == KDecl ->
              prepareDeclaration stageDirectory initial slots
            _ -> prepareSegment stageDirectory offset initial slots
          prepareStages preparation (output:reversed)
    recipe = worldCellPrepareRecipe question
    directory = worldCellPrepareDirectory question
    parserFlags = boundParserFlags bound
    text = encodeString . T.pack
    digestText = SHA256.hash . TE.encodeUtf8 . T.pack
    base = rawWorldID (worldCellPrepareIdentity question)
    planDigest = SHA256.hash (worldCellPreparePlan question)
    admission = worldCellPrepareAdmission question
    admissionHex = concatMap (\byte -> [intToDigit (fromIntegral byte `div` 16),intToDigit (fromIntegral byte `mod` 16)])
      (BS.unpack admission)
    refusalTag WorldCellUnsupported = 0
    refusalTag WorldCellUnavailable = 1
    refusalTag WorldCellCorrupt = 2
    writeResult outcome observations inputs common plans stages = BS.writeFile (worldCellPrepareOutput question) $
      toStrictByteString (encodeListLen 10 <> text "TPCWPREP4" <> encodeBytes base
        <> encodeBytes planDigest <> encodeBytes admission
        <> (case outcome of
          Left refusal -> encodeListLen 2 <> text "rejected" <> encodeWord64 (refusalTag refusal)
          Right items -> encodeListLen 2 <> text "prepared" <> encodeListLen (fromIntegral (length items))
            <> foldMap (\(ordinal,generation,sha) -> encodeListLen 3 <> encodeWord64 ordinal
              <> encodeWord64 generation <> encodeBytes sha)
              (map preparedWorldCellItemManifest items))
        <> encodeBytes observations
        <> encodeListLen (fromIntegral (length inputs))
        <> foldMap encodeWorldCellInputReference inputs
        <> encodeListLen (fromIntegral (length common))
        <> foldMap (\(path,sha) -> encodeListLen 2 <> text path <> encodeBytes sha) common
        <> encodeListLen (fromIntegral (length plans)) <> foldMap encodeTypedSegmentPlan plans
        <> encodeListLen (fromIntegral (length stages)) <> mconcat stages)
    prepareSegment stageDirectory offset initial matchedSlots = do
      createDirectoryIfMissing True stageDirectory
      preparedBatch <- newIORef Nothing
      attempt <- newIORef (0 :: Int)
      let template = worldCellTemplate recipe
      checkingTemplate <- either (throwIO . InvalidCheckingWrapper) pure
        (replaceTemplateModuleHeader "module CellCheck where" template)
      let slots = [(fromIntegral ordinal,generation,observation) | (ordinal,slot) <- matchedSlots
            , (generation,observation) <- case slot of
                PlannedBind value -> [(value,Nothing)]
                PlannedExpression value name -> [(value,Just name)]
                _ -> []]
          compile plan = do
            index <- readIORef attempt
            modifyIORef' attempt (+1)
            writeIORef preparedBatch Nothing
            let stagingRoot = stageDirectory </> "typed-session-" ++ show index
            source <- either fail pure (prepareTypedSegmentSource checkingTemplate plan admissionHex slots)
            let owner = generatedScaffoldModuleName ["held-world-cell-v1",worldCellPrepareIdentity question
                  ,shaHex (worldCellPreparePlan question),shaHex admission,preparedTypedSegmentSource source]
            effective <- templateParserFlags parserFlags (preparedTypedSegmentSource source) >>= either throwCellSplitError pure
            named <- either fail pure (renameScaffoldModuleHeader effective owner checkingTemplate)
            authored <- either fail pure (prepareTypedSegmentSource named plan admissionHex slots)
            unless (preparedTypedSegmentPlan source == preparedTypedSegmentPlan authored)
              (throwIO WorldCellCorrupt)
            let protected = preparedTypedSegmentTemplate authored
                rendered = preparedTypedSegmentSource authored
                typedPlan = preparedTypedSegmentPlan authored
                path = stageDirectory </> owner ++ ".hs"
            imports <- either fail pure (captureProtectedTemplateImportsAt "{{CELL_IMPORTS}}" parserFlags protected)
            mapped <- either fail pure (renderProtectedTemplateImports (preparedTypedSegmentLineOffset authored) imports)
            writeFile path rendered
            scaffold <- generatedTypedSegmentRecipeWithProtectedImports (preparedTypedSegmentOperations authored) parserFlags
              (CheckedTemplateImports [] []) mapped protected rendered path owner >>= either fail pure
            let prepare environment admissions typed = do
                  batch <- prepareTypedSegmentSessionBindings environment admissions typed stagingRoot
                  writeIORef preparedBatch (Just batch)
                  pure (typedSegmentSessionEnvironment batch,typedSegmentSessionGlobals batch,typedSegmentSessionInterfaces batch)
                purpose = withSourceImportIntents (cellPlanPrologue plan) $
                  GeneratedScaffoldCompile scaffold (GeneratedInstanceCheck (cellGeneratedInstanceRecipe plan)
                    (TypedSegmentCompile typedPlan (preparedTypedSegmentOperations authored) (CheckedItemCompile [] Nothing [])))
            completed <- compilePreparedSegmentRequest bound
              (WithTypedSegmentPreparation prepare (PreparedSegmentProducts typedPlan Nothing)) Set.empty purpose Nothing path
            pure (authored,completed)
      (finalized,(authored,(completion,typed))) <- checkCellInstances compile initial
      batch <- readIORef preparedBatch >>= maybe (fail "world cell has no actual typed batch") pure
      withPreparedCompilation completion $ \prepared _ _ _ -> do
        let result = pprPipelineResult prepared
            typedPlan = preparedTypedSegmentPlan authored
            rendered = preparedTypedSegmentSource authored
            items = typedSegmentItems typed
            targets = map (plannedItemEntry . typedItemPlan) items
            binds = prBinds result
            targetOwner = tcg_mod (prTargetTcGblEnv result)
        actualTarget <- case [value | value <- pprModules prepared,pmModule value == targetOwner] of
          [value] -> pure value
          _ -> fail "world cell completion has no unique actual target"
        captures <- either throwIO pure (resolveTypedSegmentBindings (pmIdentityResolver actualTarget) batch)
        signaturesByItem <- forM items $ \item -> do
          signatures <- forM (typedItemCaptures item) $ \capture ->
            captureCheckedSignature (prHscEnv result)
              (plannedItemEntry (typedItemPlan item) ++ ":"
                ++ occNameString (nameOccName (idName (typedCaptureIdentifier capture)))) (typedCaptureType capture)
          pure (plannedItemOrdinal (typedItemPlan item),signatures)
        originalInterfaces <- newPreparedOriginalInterfaceArtifacts prepared stageDirectory
        let producedGenerations = Map.fromList [(preparedRootIdentity global,generation)
              | (global,generation) <- typedSegmentSessionRetainedGlobals batch]
            retainedGenerations = maybe Map.empty worldCellRetainedGenerations (preparedCompilationWorldInputs completion)
        unless (Map.null (Map.intersection producedGenerations retainedGenerations))
          (throwIO WorldCellCorrupt)
        let generations = Map.union producedGenerations retainedGenerations
        (artifacts,context) <- prepareArtifacts originalInterfaces stageDirectory completion targets (standardAuxiliaryRoots binds)
          generations (typedSegmentSessionBindingRepresentations batch)
        correlated <- correlateWorldCellItems items artifacts (zip [offset..] (cellPlanItems finalized)) captures
          signaturesByItem
        products <- maybe (fail "world cell has no actual original inventory") pure context
        staged <- writeCertifiedSegmentProducts (importPaths (hsc_dflags (prHscEnv result))) originalInterfaces stageDirectory prepared products
        preparedItems <- forM correlated $ \correlatedItem -> do
          let item = worldCellItemTyped correlatedItem
              sourceItem = worldCellItemParsed correlatedItem
              artifact = worldCellItemArtifact correlatedItem
              selected = worldCellItemCaptures correlatedItem
              itemSignatures = worldCellItemSignatures correlatedItem
              itemPlan = typedItemPlan item
              ordinal = plannedItemOrdinal itemPlan
              generation = plannedItemGeneration itemPlan
              child = directory </> "item-" ++ show ordinal
              output = artifact {paTarget=preparedScaffoldTargetName}
              turn = TBind (map (T.pack . bbName) selected) 0 selected (paYieldSites output) (T.pack rendered)
          createDirectoryIfMissing True child
          sidecarFiles <- writePreparedSidecars InlineYieldSites child binds Nothing (map T.pack (prWarnings result)) [output]
          programFiles <- writePreparedArtifacts child [output]
          productFile <- writeCertifiedSegmentItemProducts prepared staged child (paProgram output)
          let turnFile = "turn.cbor"
          BS.writeFile (child </> turnFile) (encodeTurnOut turn)
          valueFiles <- case worldCellItemInterface correlatedItem of
            Nothing -> pure []
            Just value -> case capturedSessionInterfaceEvidence value of
              Just (packages,requirements,_) -> do
                let (_,interface) = capturedSessionInterface value
                forM_ [("value.hi",interface),("value.hi.packages",packages),("value.hi.requirements",requirements)] $ \(name,bytes) ->
                  BS.writeFile (child </> name) bytes
                pure ["value.hi","value.hi.packages","value.hi.requirements"]
              Nothing -> fail "world value output lacks its actual package/requirement capture"
          nativeOrdinal <- stagedOriginalEntryOrdinal staged (preparedRootIdentity (typedItemRoot item))
          files <- forM (programFiles ++ sidecarFiles ++ [turnFile,productFile] ++ valueFiles) $ \name -> do
            bytes <- BS.readFile (child </> name)
            pure (name,SHA256.hash bytes)
          let description = toStrictByteString (encodeListLen 13 <> text "TPCWITEM2"
                <> encodeBytes base <> encodeBytes planDigest <> encodeBytes admission
                <> encodeWord64 (fromIntegral ordinal) <> encodeWord64 generation
                <> text (typedSegmentPlanDigest typedPlan) <> encodeBytes (digestText (cellAnalysisSource sourceItem))
                <> text (plannedItemEntry itemPlan) <> encodeSymbol (preparedRootIdentity (typedItemRoot item))
                <> encodeWord64 (fromIntegral nativeOrdinal)
                <> encodeListLen 2
                <> encodeListLen (fromIntegral (length selected)) <> foldMap encodeBoundBinder selected
                <> encodeListLen (fromIntegral (length itemSignatures))
                <> foldMap encodeCheckedSignature itemSignatures
                <> encodeListLen (fromIntegral (length files))
                <> foldMap (\(name,sha) -> encodeListLen 2 <> text name <> encodeBytes sha) files)
          BS.writeFile (child </> "world-item.cbor") description
          let observation = fmap (\value -> CellExpressionPlan (plannedItemEntry itemPlan)
                (case typedObservationLift value of PureObservation -> ExpressionPure; EffectfulObservation -> ExpressionEffectful)
                (showSDoc (hsc_dflags (prHscEnv result)) (ppr (typedObservationValueType value)))
                (nominalHeadsOfType (typedObservationValueType value))) (typedItemObservation item)
          pure (PreparedWorldCellItem (fromIntegral ordinal,generation,SHA256.hash description) observation)
        -- Actual certification completes before exposing any private item table.
        -- Nothing here adopts a runtime value or updates an actor-current world.
        _ <- publishStagedOriginalProducts prepared staged
        let identity = encodeListLen 2 <> text "exec" <> text (typedSegmentPlanDigest typedPlan)
        finishStage stageDirectory identity (map (plannedItemOrdinal . typedItemPlan) items)
          finalized rendered preparedItems [typedPlan] completion prepared staged
    prepareDeclaration stageDirectory initial matchedSlots = do
      (ordinal,generation,kind,sourceItem) <- case (matchedSlots,cellPlanItems initial) of
        ([(ordinal,PlannedDeclaration generation)],[item]) -> pure (ordinal,generation,"declaration",item)
        ([(ordinal,PlannedPrologue generation)],[item]) -> pure (ordinal,generation,"prologue",item)
        _ -> throwIO WorldCellCorrupt
      wrapper <- either fail pure (originalDeclarationWrapper (worldCellTemplate recipe))
      let reserved = sessionModuleString (SessionModule LibMod (Generation generation))
          planned plan = either (fail . show) pure (preparePlannedDeclaration reserved wrapper plan)
          sourcePath = stageDirectory </> reserved ++ ".hs"
          compile plan = do
            original <- planned plan
            writeFile sourcePath (plannedSource original)
            imports <- either fail pure (captureProtectedTemplateImportsAt "{{CELL_IMPORTS}}" parserFlags wrapper)
            let pragmaLines = sum [length (lines (locatedPragmaSource pragma))
                  | pragma <- prologuePragmas (cellPlanPrologue plan)]
            mapped <- either fail pure (renderProtectedTemplateImports pragmaLines imports)
            scaffold <- generatedCheckingTemplateRecipeWithProtectedImports parserFlags
              (CheckedTemplateImports [] []) mapped wrapper (plannedSource original) sourcePath reserved
                >>= either fail pure
            completion <- compilePreparedRequest bound (PreparedProducts Nothing) Set.empty
              (withSourceImportIntents (cellPlanPrologue plan)
                (GeneratedScaffoldCompile scaffold (GeneratedInstanceCheck (cellGeneratedInstanceRecipe plan)
                  (WorldPlannedDeclarationCompile original)))) Nothing sourcePath
            pure (original,completion)
      createDirectoryIfMissing True stageDirectory
      (finalized,(submitted,completion)) <- checkCellInstances compile initial
      withPreparedCompilation completion $ \prepared _ _ _ -> do
        let result = pprPipelineResult prepared
        (original,inventory) <- maybe (fail "world declaration lost its actual certified stage inventory") pure
          (preparedCompilationPlannedDeclaration completion)
        unless (plannedModule original == reserved && plannedSource original == plannedSource submitted)
          (throwIO WorldCellCorrupt)
        interfaces <- newPreparedOriginalInterfaceArtifacts prepared stageDirectory
        let generations = maybe Map.empty worldCellRetainedGenerations (preparedCompilationWorldInputs completion)
        -- The existing original writer uses its real checking root to obtain
        -- the native inventory. No executable item or target packet is emitted.
        (_,context) <- prepareArtifacts interfaces stageDirectory completion [scaffoldTargetName] [] generations []
        products <- maybe (fail "world declaration has no actual original inventory") pure context
        staged <- writeCertifiedSegmentProducts (importPaths (hsc_dflags (prHscEnv result)))
          interfaces stageDirectory prepared products
        certified <- publishStagedOriginalProducts prepared staged
        let (unit,owner) = plannedOriginalOwner inventory
        originalProduct <- case [product | product <- certifiedOriginalProducts certified
            , let (actualUnit,actualOwner,_,_) = moduleProductInput product
            , (T.unpack actualUnit,T.unpack actualOwner) == (unit,owner)] of
          [product] -> pure product
          _ -> fail "world declaration has no unique certified original"
        let (_,_,interfaceBytes,_) = moduleProductInput originalProduct
        local <- maybe (fail "world declaration lacks its captured finalization") pure
          (Map.lookup (unit,owner) (finalizedLocalAdmissions (certifiedFinalizedArtifacts certified)))
        when (isNothing (localFinalizedCore local))
          (fail "world declaration lacks its finalized Core")
        let (iface,_,_) = localFinalizedInterface local
        unless (exactSha256 iface == shaHex interfaceBytes) (throwIO WorldCellCorrupt)
        _ <- either fail pure (admitLocalNativeDeclaration original inventory local)
        let identity = encodeListLen 4 <> text kind <> encodeWord64 ordinal <> text unit <> text owner
            description = toStrictByteString (encodeListLen 8 <> text "TPCWORIGINAL1"
              <> encodeBytes base <> encodeBytes planDigest <> encodeBytes admission
              <> encodeWord64 ordinal <> encodeWord64 generation
              <> encodeBytes (digestText (cellAnalysisSource sourceItem))
              <> encodeListLen 5 <> text kind <> text unit <> text owner
              <> encodeBytes (SHA256.hash interfaceBytes) <> text (renderPlannedDeclarationInventory inventory))
            child = directory </> "item-" ++ show ordinal
        createDirectoryIfMissing True child
        BS.writeFile (child </> "world-item.cbor") description
        let item = PreparedWorldCellItem (ordinal,generation,SHA256.hash description) Nothing
        finishStage stageDirectory identity [fromIntegral ordinal] finalized (plannedSource original)
          [item] [] completion prepared staged
    finishStage stageDirectory identity ordinals finalized rendered preparedItems plans completion prepared staged = do
        validateDependencyEvidence (preparedFreshDependencies prepared)
        inputFacts <- maybe (fail "world cell completion lost its actual input basis") pure
          (preparedCompilationWorldInputs completion)
        forM_ (worldCellSourceSelection inputFacts) $ \selected -> do
          unchanged <- revalidateSourceSelectedOriginals selected
          unless unchanged (throwIO WorldCellUnavailable)
        let (sourcePath,sourceSha) = worldCellSourceSnapshot inputFacts
            commonWrites = (sourcePath,sourceSha)
              : [(path,rawWorldID sha) | (path,sha) <- stagedOriginalOutputFiles staged]
        owner <- canonicalizePath directory
        common <- forM commonWrites $ \(path,sha) -> do
          actual <- canonicalizePath path
          let relative = makeRelative owner actual
          unless (not (isAbsolute relative) && not (null relative)
              && all (`notElem` [".",".."]) (splitDirectories relative) && BS.length sha == 32)
            (throwIO WorldCellCorrupt)
          pure (relative,sha)
        let sourceName = makeRelative owner sourcePath
            stageName name = makeRelative owner (stageDirectory </> name)
            fileRef name = case lookup name common of
              Just sha -> encodeListLen 2 <> text name <> encodeBytes sha
              Nothing -> error "world source receipt lacks its actual common output"
            importRow (qualifier,name,boot,unit) = encodeListLen 4
              <> text (renderDependencyQualifier qualifier) <> text name <> encodeBool boot <> text unit
            moduleRow ((unit,name,boot),edges) = encodeListLen 4
              <> text unit <> text name <> encodeBool boot
              <> encodeListLen (fromIntegral (length edges)) <> foldMap importRow edges
            sourceEnvelope = toStrictByteString (encodeListLen 11 <> text "TPCWSOURCE3"
              <> encodeBytes base <> encodeBytes planDigest <> encodeBytes admission
              <> identity
              <> fileRef sourceName <> fileRef (stageName "dependencies.json") <> fileRef (stageName "module-package-imports.cbor")
              <> encodeListLen (fromIntegral (length (worldCellExactImports inputFacts)))
              <> foldMap moduleRow (worldCellExactImports inputFacts)
              <> encodeSourceSelectedOriginals (worldCellSourceSelection inputFacts)
              <> encodeListLen (fromIntegral (length (worldCellSourceRootAliases inputFacts)))
              <> foldMap text (worldCellSourceRootAliases inputFacts))
            sourceReceiptName = stageName "world-source.cbor"
        BS.writeFile (directory </> sourceReceiptName) sourceEnvelope
        let receiptRef = encodeListLen 2 <> text sourceReceiptName <> encodeBytes (SHA256.hash sourceEnvelope)
            stageReceipt = encodeListLen 3 <> identity
              <> encodeListLen (fromIntegral (length ordinals)) <> foldMap (encodeWord64 . fromIntegral) ordinals
              <> encodeListLen 3 <> receiptRef
              <> fileRef (stageName "module-products.cbor") <> fileRef (stageName "certified-products.cbor")
        pure PreparedWorldCellStage
          { preparedWorldStageItems = preparedItems
          , preparedWorldStagePlan = finalized
          , preparedWorldStageSource = rendered
          , preparedWorldStageInputs = worldCellInputReferences inputFacts
          , preparedWorldStageFiles = (sourceReceiptName,SHA256.hash sourceEnvelope):common
          , preparedWorldStageTypedPlans = plans
          , preparedWorldStageReceipt = stageReceipt
          , preparedWorldStageCompletion = completion
          }
    rawWorldID [] = BS.empty
    rawWorldID (a:b:rest) = BS.cons (fromIntegral (16 * digitToInt a + digitToInt b)) (rawWorldID rest)
    rawWorldID _ = error "world identity left its admitted shape"
