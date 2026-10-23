{-# LANGUAGE GADTs #-}
{-# LANGUAGE RankNTypes #-}

-- | Request orchestration owns normalized admission and the executor allowance.
-- Completed preparation retains its actual recovery references and local worklist;
-- later compiles and helper operations cannot replace either.
module Tidepool.CompilerRequest
  ( BoundCompilerRequest, withBoundCompilerRequest, withBoundCompilerRequestWithNativeDemand, boundWorkerRequest
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
import Control.Monad ( foldM, forM, forM_, unless, when)
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
  ( writeCertifiedSegmentProducts, writeCertifiedSegmentItemProductsWithInputs
  , CompletedWorldProducts, completeWorldProducts, completedWorldNative
  , WorldStageInputs, exchangeWorldNativeDemand, bindWorldStageInputsWithNativeDemand, requireWorldStageInputs, withWorldStageBasis, worldStagePriorOriginals, withWorldStageBindings, withWorldStageCompilation, newPreparedWorldInterfaceArtifacts
  , stagedOriginalEntryOrdinal, stagedOriginalOutputFiles, publishStagedOriginalProducts
  , certifiedOriginalProducts, certifiedFinalizedArtifacts
  , StagedOriginalProducts, stagedOriginalBinders, stagedOriginalExternalDemand, stagedOriginalReference, withWorldStageInputs
  , stagedDeclarationInputReferences
  , OriginalProductWorklist, observeOriginalProjectionWithRecovery, PreparedProductContext
  , prepareOriginalProductsWithCacheAndReuse, prepareOriginalProductsWithWorklist
  , prepareCompilerProjectionContext, requireOriginalExecutableGlobals, admitCurrentOriginalProducts, retainPreparedPackageTargets
  , preparedCurrentOriginalInventory, preparedProductInventory, currentOriginalBinders
  , currentOriginalBindingsExcept, currentReconciledOriginalProducts )
import Tidepool.DiagJson (InputRejection(..), SourceRejection(..))
import Tidepool.ExactHydration (ExactIfaceArtifact(..), ExactInterfaceOperations, OriginalInterfaceArtifacts)
import Tidepool.ImportAdmission (compilationScope)
import Tidepool.ExactScope
import Tidepool.ExecutionSchema
  ( SymbolIdentity(..), WireProgram(..), SiteRow(..), ProjectedGroup(..), GlobalDecl(..) )
import qualified Tidepool.ExecutionSchema as Execution
import Tidepool.ExecutionEncode (encodeWireProgram, moduleProductInput, projectedGroupEncodingInput)
import Tidepool.ExecutionProjection
  ( ProjectionContext(..), ProjectionError(..), prepareComponentProjectionWithReachability
  , projectSelectedCandidateWithHostBindings, candidateGlobals, candidateBindingIdentities
  , finalizePreparedCandidate
  , preparedModuleProductConstructors, preparedModuleProductYieldSites, preparedModuleProductOutcomes
  , preparedRootIdentity, PreparedPackageTarget, projectPreparedPackageTarget
  , packageTargetName, packageTargetProgram, packageTargetConstructors, packageTargetGroups, packageTargetImports
  , projectedPackageModule, projectedPackageInterfaceSha256, projectedPackageEncoding
  , projectedPackageConstructors, projectedPackageYieldSites )
import Tidepool.HostBindingAuthority (HostBindingRepresentation, hostBindingRepresentationJsonAuthority)
import Tidepool.ConstructorIdentity (IssuedConstructor)
import qualified Tidepool.EffectSchema
import Tidepool.CheckedCell (sealCheckedTypeWitness, CheckedSignature, captureCheckedSignature, encodeCheckedSignature)
import Tidepool.CertifiedProducts (resolvePackageGlobal, encodeReceiptIdentity)
import Tidepool.ModuleCandidates (ModuleCandidate(..), CandidateGroup(..))
import Tidepool.WorkerServer (NativeDemandExchange)
import Tidepool.DependencyPlan
import Tidepool.OriginalProductRoots
  ( PriorOriginalExternalDemand(..), originalExternalPackageGlobals
  , emptyOriginalExternalDemand, requiredOriginalExternalDemandForSeeds
  , unrecoveredExactProducts )
import Tidepool.PreparedStg (PreparedModule, pmModule, pmYieldSites, pmIdentityResolver
  , preparedComponentVersion, preparedComponentModules, capturePreparedPackageInterfaces)
import Tidepool.FatIface (fatOriginalOwner)
import Tidepool.ArtifactBytes (ArtifactBytes, artifactSha256)
import Tidepool.PackageWitness (PackageImportEvidence(..), PackageImportRoot(..))
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
import Data.Char (digitToInt)
import Data.List (intercalate)
import System.Directory (createDirectoryIfMissing, canonicalizePath)
import System.FilePath ((</>), makeRelative, isAbsolute, splitDirectories)
import Codec.CBOR.Encoding (Encoding, encodeBytes, encodeListLen, encodeString, encodeWord64, encodeBool, encodeInt)
import Codec.CBOR.Write (toStrictByteString)
import GHC.Core (Bind(..), CoreBind)
import GHC.Types.Id (idName)
import GHC.Types.Name (nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Driver.Env (hsc_dflags, hsc_home_unit)
import GHC.Unit.Home (isHomeUnit)
import GHC.Unit.Module (Module, mkModule, mkModuleName)
import GHC.Unit.Types (stringToUnit)
import GHC.Driver.Ppr (showSDoc)
import GHC.Utils.Outputable (ppr)
import Tidepool.Binders
  ( BoundBinder(..), CellSourcePlan(..), CellAnalysisItem(..), CellExpressionPlan(..), ExpressionLiftPlan(..)
  , PreparedTypedSegmentSource(..), TurnOut(..), renderAskJson
  , StmtBinders(..), TurnKind(..) )
import Tidepool.CborEncode (encodeMetadata, encodeTurnOut, encodeCellOut, encodeBoundBinder, encodeTypedSegmentPlan)
import Tidepool.ExecutionEncode (encodeSymbol)
import Tidepool.Metadata (metadataForConstructors, targetBindingHasIO)
import Tidepool.TypePolicy (nominalHeadsOfType)
import Tidepool.ExtractUtil (shaHex, trySynchronous)
import Tidepool.DependencyEvidence (validateDependencyEvidence, renderDependencyQualifier)
import Tidepool.PlannedDeclaration
  ( plannedModule, plannedSource, plannedOriginalOwner, renderPlannedDeclarationInventory
  , PlannedDeclaration, PlannedDeclarationInventory, admitLocalNativeDeclaration )
import Tidepool.FinalizedModuleArtifacts
  ( finalizedLocalAdmissions, localFinalizedCore, localFinalizedInterface )
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
  AdmittedCompilerOperation CompilerExecutionGrant CompilerExecutor (Maybe CellCompilationInputs) (Maybe NativeDemandExchange)

-- One operation shape after shape admission and exact/held acquisition. A
-- World preparation retains its validated immutable plan; a failed acquisition
-- is a typed refusal, not an admitted operation with absent fields.
data AdmittedCompilerOperation
  = AdmittedExactOperation (Maybe ExactScope) RequestOperation
  | AdmittedHeldQueryOperation (Maybe HeldWorldQueryBinding) WorldQueryRequest
  | AdmittedHeldCellPlanOperation (Maybe HeldWorldQueryBinding) WorldCellPlanRequest
  | AdmittedHeldCellPrepareOperation HeldWorldCellPreparation

data HeldWorldCellPreparation
  = HeldWorldCellRefused WorldCellPrepareRequest WorldCellRefusal
  | HeldWorldCellAdmitted HeldWorldCellAdmission

boundWorkerRequest :: BoundCompilerRequest -> WorkerRequest
boundWorkerRequest (BoundCompilerRequest _ args _ _ _ _ _) = args

boundRequestOperation :: BoundCompilerRequest -> RequestOperation
boundRequestOperation (BoundCompilerRequest _ _ admitted _ _ _ _) = admittedTag admitted
  where
    admittedTag (AdmittedExactOperation _ operation) = operation
    admittedTag (AdmittedHeldQueryOperation _ question) = WorldQueryOperation question
    admittedTag (AdmittedHeldCellPlanOperation _ question) = WorldCellPlanOperation question
    admittedTag (AdmittedHeldCellPrepareOperation preparation) =
      WorldCellPrepareOperation (heldPreparationQuestion preparation)
    heldPreparationQuestion (HeldWorldCellRefused question _) = question
    heldPreparationQuestion (HeldWorldCellAdmitted admission) = heldWorldCellQuestion admission

boundRequestExactScope :: BoundCompilerRequest -> Maybe ExactScope
boundRequestExactScope (BoundCompilerRequest _ _ admitted _ _ _ _) = case admitted of
  AdmittedExactOperation exact _ -> exact
  _ -> Nothing

boundParserFlags :: BoundCompilerRequest -> DynFlags
boundParserFlags (BoundCompilerRequest scope _ _ _ _ _ _) = scopedParserFlags scope

withBoundCompilerRequest :: PhaseSink -> CompilerScope -> WorkerRequest -> (BoundCompilerRequest -> IO a) -> IO (Completion a)
withBoundCompilerRequest sink compilerScope parsed =
  withBoundCompilerRequestWithNativeDemand sink Nothing compilerScope parsed

withBoundCompilerRequestWithNativeDemand :: PhaseSink -> Maybe NativeDemandExchange -> CompilerScope
  -> WorkerRequest -> (BoundCompilerRequest -> IO a) -> IO (Completion a)
withBoundCompilerRequestWithNativeDemand sink native compilerScope parsed use = do
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
        WorldQueryOperation question -> AdmittedHeldQueryOperation <$>
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
      result <- use (BoundCompilerRequest compilerScope args admitted grant executor Nothing native)
      emitPhase sink BodyFinished
      pure result

-- A cell's issued lexical prefix changes only these compilation inputs. The
-- request's grant, physical search inputs and auxiliary owner remain bound.
data CellCompilationInputs = CellCompilationInputs (Maybe ProgramSourceImports)
  [((String,String),String)] [CompletedValueImport]

cellSegmentRequest :: BoundCompilerRequest -> ProgramCellState ExactScope -> BoundCompilerRequest
cellSegmentRequest (BoundCompilerRequest _ _ (AdmittedHeldQueryOperation _ _) _ _ _ _) _ = throw CheckedPurposeMismatch
cellSegmentRequest (BoundCompilerRequest _ _ (AdmittedHeldCellPlanOperation _ _) _ _ _ _) _ = throw CheckedPurposeMismatch
cellSegmentRequest (BoundCompilerRequest _ _ (AdmittedHeldCellPrepareOperation _) _ _ _ _) _ = throw CheckedPurposeMismatch
cellSegmentRequest (BoundCompilerRequest scope args (AdmittedExactOperation _ operation) grant executor _ native) state =
  let exact = programInput state
      values = map exactModule (maybe [] checkedValueInterfaces (scopeCheckedCell exact))
      inputs = CellCompilationInputs (programSourceImports state) (programOriginals state)
        (selectedProgramValues (programValues state))
  in BoundCompilerRequest scope (args {requestInjectVals=values,requestRetainedGenerations=programRetained state})
    (AdmittedExactOperation (Just exact) operation) grant executor (Just inputs) native

cellCompilationPurpose :: CellCompilationInputs -> CompilePurpose -> CompilePurpose
cellCompilationPurpose (CellCompilationInputs imports originals values) = withProgramSourceImports imports . replacePurpose
  where
    replacePurpose purpose = case purpose of
      GeneratedScaffoldCompile recipe inner -> GeneratedScaffoldCompile recipe (replacePurpose inner)
      GeneratedInstanceCheck recipe inner -> GeneratedInstanceCheck recipe (replacePurpose inner)
      TypedSegmentCompile plan operations inner -> TypedSegmentCompile plan operations (replacePurpose inner)
      OriginalDeclarationCompile -> ProgramItemCompile True [] originals values
      CheckedItemCompile annotations _ selected -> ProgramItemCompile False annotations originals selected
      other -> other

compileBoundCompleted :: BoundCompilerRequest -> PipelineSelection result
  -> Set.Set SymbolIdentity -> CompilePurpose -> Maybe SessionScope -> FilePath
  -> IO (CompletedCompilerResult result)
compileBoundCompleted (BoundCompilerRequest scope args admitted grant executor cell _) selection retained purpose session path = do
  requireCompilerExecutorOpen executor
  let effectivePurpose = maybe purpose (`cellCompilationPurpose` purpose) cell
  normalizedPurpose <- either throwIO pure (normalizeCompilePurpose effectivePurpose)
  case admitted of
    AdmittedExactOperation exact _ -> scopedCompileCompleted scope (WithCompilerExecution grant executor selection) retained
      (maybe normalizedPurpose (ExactScopeCompile normalizedPurpose) exact) session path
      (requestIncludes args) (requestBuildProductsDir args)
    AdmittedHeldQueryOperation _ _ -> throwIO CheckedPurposeMismatch
    AdmittedHeldCellPlanOperation _ _ -> throwIO CheckedPurposeMismatch
    AdmittedHeldCellPrepareOperation _ -> throwIO CheckedPurposeMismatch

-- World source and purpose are issued by the admitted stage. The ordinary
-- request helper cannot supply substitutes for either through its raw inputs.
compileBoundWorldCompleted :: BoundCompilerRequest -> PipelineSelection result -> IO (CompletedCompilerResult result)
compileBoundWorldCompleted (BoundCompilerRequest scope _ admitted _ executor _ _) selection = do
  requireCompilerExecutorOpen executor
  case admitted of
    AdmittedHeldCellPrepareOperation (HeldWorldCellRefused _ refusal) -> throwIO refusal
    AdmittedHeldCellPrepareOperation (HeldWorldCellAdmitted admission) ->
      scopedCompileWorldCell scope admission selection Set.empty
    _ -> throwIO CheckedPurposeMismatch

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
  | PreparedWorldCompletion CompiledWorldStage (Maybe NativeDemandExchange)

data PreparedCompilation = PreparedCompilation PreparedPipelineResult CompilerRecoveryCaches
  CompilerExecutor (IO ()) (Maybe OriginalProductWorklist) PreparedCompletion !PreparedOutputAuthority

data PreparedOutputAuthority
  = NativeOriginalAuthority !NativeProductProvenance
  | PortableAuthority

preparedCompilationOutputIntent :: PreparedCompilation -> PreparedOutputIntent
preparedCompilationOutputIntent (PreparedCompilation _ _ _ _ _ _ authority) = case authority of
  NativeOriginalAuthority _ -> NativeOriginals
  PortableAuthority -> PortableExecutable

preparedCompilationNativeProvenance :: PreparedCompilation -> IO NativeProductProvenance
preparedCompilationNativeProvenance (PreparedCompilation _ _ _ _ _ _ authority) = case authority of
  NativeOriginalAuthority provenance -> pure provenance
  PortableAuthority -> throwIO CheckedPurposeMismatch

acquirePreparedOutputAuthority :: BoundCompilerRequest -> IO PreparedOutputAuthority
acquirePreparedOutputAuthority (BoundCompilerRequest scope args _ _ executor _ _) = do
  requireCompilerExecutorOpen executor
  case preparedOutputIntent args of
    PortableExecutable -> pure PortableAuthority
    NativeOriginals -> NativeOriginalAuthority <$> scopedNativeProductProvenance scope

preparedCompilationResult :: PreparedCompilation -> PreparedPipelineResult
preparedCompilationResult (PreparedCompilation result _ _ _ _ _ _) = result

preparedCompilationWorldInputs :: PreparedCompilation -> Maybe WorldCellInputs
preparedCompilationWorldInputs (PreparedCompilation _ _ _ _ _ PreparedOrdinaryCompletion _) = Nothing
preparedCompilationWorldInputs (PreparedCompilation _ _ _ _ _ (PreparedWorldCompletion world _) _) =
  Just (worldCompilerCompletionInputs (compiledWorldStageCompletion world))

preparedCompilationPlannedDeclaration :: PreparedCompilation -> Maybe (PlannedDeclaration,PlannedDeclarationInventory)
preparedCompilationPlannedDeclaration (PreparedCompilation _ _ _ _ _ PreparedOrdinaryCompletion _) = Nothing
preparedCompilationPlannedDeclaration (PreparedCompilation _ _ _ _ _ (PreparedWorldCompletion world _) _) =
  worldCompilerCompletionPlannedDeclaration (compiledWorldStageCompletion world)

withPreparedCompilation :: PreparedCompilation
  -> (PreparedPipelineResult -> CompilerRecoveryCaches -> CompilerExecutor -> Maybe OriginalProductWorklist -> IO a)
  -> IO a
withPreparedCompilation (PreparedCompilation result recovery executor guard worklist _ _) use = do
  guard
  requireCompilerExecutorOpen executor
  use result recovery executor worklist

-- World effects borrow the bound stage's own result, caches and executor.
-- The ordinary route has no predecessor or native exchange authority.
withPreparationInputs :: PreparedCompilation -> Maybe WorldStageInputs
  -> (PreparedPipelineResult -> CompilerRecoveryCaches -> CompilerExecutor -> Maybe OriginalProductWorklist -> IO a) -> IO a
withPreparationInputs completion (Just inputs) use = case completion of
  PreparedCompilation _ _ _ _ _ (PreparedWorldCompletion stage _) _ -> do
    requireWorldStageInputs stage inputs
    withWorldStageCompilation inputs (\prepared recovery executor -> use prepared recovery executor Nothing)
  _ -> throwIO CheckedPurposeMismatch
withPreparationInputs completion Nothing use = case completion of
  PreparedCompilation _ _ _ _ _ PreparedOrdinaryCompletion _ -> withPreparedCompilation completion use
  _ -> throwIO CheckedPurposeMismatch

bindPreparedWorldInputs :: PreparedCompilation -> Maybe CompletedWorldProducts -> IO WorldStageInputs
bindPreparedWorldInputs completion prior = withPreparedCompilation completion $ \_ _ _ _ -> case completion of
  PreparedCompilation _ _ _ _ _ (PreparedWorldCompletion stage native) _ -> do
    inputs <- bindWorldStageInputsWithNativeDemand stage prior native
    provenance <- preparedCompilationNativeProvenance completion
    withWorldStageBasis inputs $ \facts -> unless
      (nativeProductProducer provenance == worldInputsProducer facts
        && nativeProductProfile provenance == worldInputsProfile facts) (throwIO WorldCellCorrupt)
    pure inputs
  _ -> throwIO CheckedPurposeMismatch

-- Projection inputs may legitimately change entry/generation/host authority.
-- The actual compiler environment, cache references and worklist cannot change.
prepareCompilationOriginals :: PreparedCompilation -> Maybe WorldStageInputs -> ProjectionContext -> Set.Set SymbolIdentity
  -> IO ([PreparedModule],PreparedProductContext)
prepareCompilationOriginals completion prior context external =
  withPreparationInputs completion prior $ \prepared recovery executor worklist ->
    prepareOriginals prepared recovery executor worklist prior context external

prepareOriginals :: PreparedPipelineResult -> CompilerRecoveryCaches -> CompilerExecutor
  -> Maybe OriginalProductWorklist -> Maybe WorldStageInputs -> ProjectionContext -> Set.Set SymbolIdentity
  -> IO ([PreparedModule],PreparedProductContext)
prepareOriginals prepared recovery executor worklist prior context external = do
  let env = prHscEnv (pprPipelineResult prepared)
      exact = compilationScope <$> preparedExactCompilation prepared
      interfaces = pprProductInterfaces prepared
      modules = pprModules prepared
  case worklist of
    Just completed -> prepareOriginalProductsWithWorklist completed env exact interfaces context external modules
    Nothing -> prepareOriginalProductsWithCacheAndReuse (Just (pprReuseContext prepared))
      (compilerPreparedBodies recovery) (Just (compilerOriginalProjections recovery)) executor
      prior
      env exact interfaces context external modules

compileWithWorklist :: BoundCompilerRequest -> PipelineSelection result
  -> Set.Set SymbolIdentity -> CompilePurpose -> Maybe SessionScope -> FilePath
  -> IO (CompletedCompilerResult result, Maybe OriginalProductWorklist)
compileWithWorklist request@(BoundCompilerRequest _ args _ _ executor _ _) selection retained purpose session path = do
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
compilePreparedRequest request@(BoundCompilerRequest _ args admitted _ executor _ _) selection retained purpose session path = do
  authority <- acquirePreparedOutputAuthority request
  (completed,worklist) <- case (admitted,preparedOutputIntent args) of
    (AdmittedHeldCellPrepareOperation _,_) -> throwIO CheckedPurposeMismatch
    (_,NativeOriginals) -> compileWithWorklist request selection retained purpose session path
    (_,PortableExecutable) -> do
      completed <- compileBoundCompleted request selection retained purpose session path
      pure (completed,Nothing)
  route <- preparedCompletionFor Nothing completed
  pure (PreparedCompilation (completedCompilerResult completed) (completedCompilerRecovery completed) executor (completedCompilerGuard completed) worklist route authority)

compilePreparedSegmentRequest :: BoundCompilerRequest -> PipelineSelection PreparedSegmentProductsResult
  -> Set.Set SymbolIdentity -> CompilePurpose -> Maybe SessionScope -> FilePath -> IO (PreparedCompilation,TypedSegment)
compilePreparedSegmentRequest request@(BoundCompilerRequest _ _ _ _ executor _ _) selection retained purpose session path = do
  authority <- acquirePreparedOutputAuthority request
  -- Typed segment preparation acquires its own Session bindings during the
  -- compiler pass. It has no early original observer; projection uses the
  -- captured recovery context and the existing fallback path.
  completed <- compileBoundCompleted request selection retained purpose session path
  let result = completedCompilerResult completed
  route <- preparedCompletionFor Nothing completed
  pure (PreparedCompilation (preparedSegmentProducts result) (completedCompilerRecovery completed) executor (completedCompilerGuard completed) Nothing route authority,
    preparedSegmentCaptures result)

compilePreparedWorldRequest :: BoundCompilerRequest -> PipelineSelection PreparedPipelineResult -> IO PreparedCompilation
compilePreparedWorldRequest request@(BoundCompilerRequest _ _ _ _ executor _ native) selection = do
  authority <- acquirePreparedOutputAuthority request
  completed <- compileBoundWorldCompleted request (maybe selection (\exchange -> WithCompilerDependencyExchange exchange selection) native)
  route <- preparedCompletionFor native completed
  pure (PreparedCompilation (completedCompilerResult completed) (completedCompilerRecovery completed)
    executor (completedCompilerGuard completed) Nothing route authority)

compilePreparedWorldSegmentRequest :: BoundCompilerRequest -> PipelineSelection PreparedSegmentProductsResult
  -> IO (PreparedCompilation,TypedSegment)
compilePreparedWorldSegmentRequest request@(BoundCompilerRequest _ _ _ _ executor _ native) selection = do
  authority <- acquirePreparedOutputAuthority request
  completed <- compileBoundWorldCompleted request (maybe selection (\exchange -> WithCompilerDependencyExchange exchange selection) native)
  let result = completedCompilerResult completed
  route <- preparedCompletionFor native completed
  pure (PreparedCompilation (preparedSegmentProducts result) (completedCompilerRecovery completed)
    executor (completedCompilerGuard completed) Nothing route authority,preparedSegmentCaptures result)

runBoundGhc :: BoundCompilerRequest -> Ghc a -> IO a
runBoundGhc (BoundCompilerRequest scope _ admitted _ executor _ _) operation = do
  requireCompilerExecutorOpen executor
  case admitted of AdmittedExactOperation _ _ -> pure (); _ -> throwIO CheckedPurposeMismatch
  scopedRunGhc scope operation

boundWorldCellPreparation :: BoundCompilerRequest -> IO (Either WorldCellRefusal HeldWorldCellAdmission)
boundWorldCellPreparation (BoundCompilerRequest _ _ admitted _ executor _ _) = do
  requireCompilerExecutorOpen executor
  case admitted of
    AdmittedHeldCellPrepareOperation (HeldWorldCellRefused _ refusal) -> pure (Left refusal)
    AdmittedHeldCellPrepareOperation (HeldWorldCellAdmitted admission) -> pure (Right admission)
    _ -> throwIO CheckedPurposeMismatch

-- Terminal output success reborrows the actual held owner. Pending completion
-- facts carry no source tree or usable compiler capability beyond scope END.
commitPreparedWorldDependencies :: BoundCompilerRequest -> PreparedCompilation -> IO ()
commitPreparedWorldDependencies (BoundCompilerRequest scope _ admitted _ executor _ _)
    (PreparedCompilation _ _ issuing guard _ route _) = do
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
          PreparedWorldCompletion world _ -> scopedCommitWorldDependencies scope admission (worldCompilerCompletionDependencies (compiledWorldStageCompletion world))
    _ -> throwIO CheckedPurposeMismatch

preparedCompletionFor :: Maybe NativeDemandExchange -> CompletedCompilerResult result -> IO PreparedCompletion
preparedCompletionFor native completed = case completedCompilerWorldStage completed of
  Just stage -> pure (PreparedWorldCompletion stage native)
  Nothing -> case completedCompilerWorldCompletion completed of
    Nothing -> pure PreparedOrdinaryCompletion
    Just _ -> throwIO CheckedPurposeMismatch

runBoundWorldCellPlan :: BoundCompilerRequest -> IO BS.ByteString
runBoundWorldCellPlan (BoundCompilerRequest scope _ admitted grant executor _ _) = do
  requireCompilerExecutorOpen executor
  case admitted of
    AdmittedHeldCellPlanOperation held question -> scopedPlanWorldCell scope held grant executor question
    _ -> throwIO CheckedPurposeMismatch

runBoundWorldQuery :: BoundCompilerRequest -> IO BS.ByteString
runBoundWorldQuery (BoundCompilerRequest scope _ (AdmittedHeldQueryOperation world question) grant executor _ _) = do
  requireCompilerExecutorOpen executor
  scopedQueryWorld scope world grant executor question
runBoundWorldQuery _ = throwIO CheckedPurposeMismatch

withBoundExactInterfaceTransaction :: BoundCompilerRequest -> (ExactInterfaceOperations -> IO a) -> IO a
withBoundExactInterfaceTransaction (BoundCompilerRequest scope args admitted _ executor _ _) use = do
  requireCompilerExecutorOpen executor
  case admitted of AdmittedExactOperation _ _ -> pure (); _ -> throwIO CheckedPurposeMismatch
  withScopedExactInterfaceTransaction scope (requestIncludes args) use

data PreparedArtifact = PreparedArtifact
  { paTarget :: String
  , paProgram :: WireProgram
  , paBytes :: BS.ByteString
  , paConstructors :: [IssuedConstructor]
  , paYieldSites :: [Tidepool.EffectSchema.YieldSite]
  , paPackageTarget :: Maybe PreparedPackageTarget
  , paDependencyTerminals :: [DependencyTerminal]
  }


data FinalProjection
  = NativeFinal !PreparedPackageTarget !(Map.Map Module ArtifactBytes) ![SymbolIdentity]
  | PortableFinal !WireProgram ![IssuedConstructor] ![SymbolIdentity]

-- Project before writing artifacts so the shared constructor
-- table includes exactly the GHC constructors admitted by prepared execution.
prepareArtifacts :: OriginalInterfaceArtifacts -> FilePath -> PreparedCompilation
  -> [String] -> [String] -> Map.Map SymbolIdentity Word64 -> [HostBindingRepresentation]
  -> IO ([PreparedArtifact], Maybe PreparedProductContext)
prepareArtifacts = prepareArtifactsWithProjectionIntent Nothing requireProjection

data PreparedOutputIntent = NativeOriginals | PortableExecutable deriving (Eq)

data PreparedArtifactOutput
  = NativeOutputs [PreparedArtifact] PreparedProductContext
  | PortableOutputs [PreparedArtifact]

-- The request chooses its output contract before module completion observers
-- run. Portable compilation keeps the same checked pipeline and executor but
-- does not observe unrelated modules as native-original export products.
preparedOutputIntent :: WorkerRequest -> PreparedOutputIntent
preparedOutputIntent args
  | requestPortableOutput args = PortableExecutable
  | otherwise = NativeOriginals

prepareArtifactsWithIntent :: OriginalInterfaceArtifacts -> FilePath -> PreparedCompilation
  -> [String] -> [String] -> Map.Map SymbolIdentity Word64 -> [HostBindingRepresentation]
  -> IO PreparedArtifactOutput
prepareArtifactsWithIntent interfaces directory completion targets auxiliary retained hostBindings = do
  (artifacts,products) <- prepareArtifactsWithProjectionIntent Nothing requireProjection
    interfaces directory completion targets auxiliary retained hostBindings
  case preparedCompilationOutputIntent completion of
    NativeOriginals -> NativeOutputs artifacts <$> maybe (fail "native output lacks its product owner") pure products
    PortableExecutable -> pure (PortableOutputs artifacts)

prepareArtifactsWithProjection
  :: (forall a. Either ProjectionError a -> IO a)
  -> OriginalInterfaceArtifacts -> FilePath -> PreparedCompilation
  -> [String] -> [String] -> Map.Map SymbolIdentity Word64 -> [HostBindingRepresentation]
  -> IO ([PreparedArtifact], Maybe PreparedProductContext)
prepareArtifactsWithProjection = prepareArtifactsWithProjectionIntent Nothing

prepareArtifactsWithProjectionIntent
  :: Maybe WorldStageInputs -> (forall a. Either ProjectionError a -> IO a)
  -> OriginalInterfaceArtifacts -> FilePath -> PreparedCompilation
  -> [String] -> [String] -> Map.Map SymbolIdentity Word64 -> [HostBindingRepresentation]
  -> IO ([PreparedArtifact], Maybe PreparedProductContext)
prepareArtifactsWithProjectionIntent inputs _ _ _ completion [] _ _ _ =
  withPreparationInputs completion inputs (\_ _ _ _ -> pure ([], Nothing))
prepareArtifactsWithProjectionIntent prior project originalInterfaces outDir completion targets@(firstTarget : _) auxiliaryRoots retainedGenerations hostBindings =
  withPreparationInputs completion prior $ \prepared recoveryCaches executor _ -> do
    timing <- readTimingEnabled
    let intent = preparedCompilationOutputIntent completion
        result = pprPipelineResult prepared
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
    baseContext <- prepareCompilerProjectionContext prepared retainedGenerations (pmModule preparedModule)
      firstTarget auxiliaryRoots (case mapMaybe hostBindingRepresentationJsonAuthority hostBindings of
        authority : _ -> Just authority
        [] -> Nothing)
    let priorNative = prior >>= worldStagePriorOriginals
    firstContext <- maybe (pure baseContext) (\inputs -> withWorldStageBindings inputs baseContext) prior
    let contextFor target = firstContext
          { projectionEntry = (projectionEntry firstContext) {symbolOccurrence = T.pack target} }
    let exactProducts = maybe [] scopeAvailableOriginalProducts exactScope
        externalOriginalBinders = Set.unions
          [maybe Set.empty stagedOriginalBinders priorNative, Map.keysSet (pprOriginalReferences prepared), Set.fromList
          ([binder | exactProduct <- exactProducts, group <- originalGroups exactProduct
            , binder <- originalBinders group]
           ++ [binder | candidate <- candidates, group <- candidateGroups candidate
            , binder <- candidateGroupBinders group])]
    (originalModules,productContext,originalBindings,admittedOriginalBinders,
      originalConstructors,originalSites,originalPackageGlobals,homeProductGroups) <- case intent of
      PortableExecutable -> do
        unless (isNothing exactScope && null candidates && Map.null retainedGenerations
            && null hostBindings) $
          fail "portable output cannot consume retained native or host-binding inputs"
        pure (modules,Nothing,Map.empty,Set.empty,[],[]
          ,const (const (Right emptyOriginalExternalDemand)),Map.empty)
      NativeOriginals -> do
        (originalModules,rawProductContext) <- timePhase timing "prepared_original_demand" $
          prepareCompilationOriginals completion prior (contextFor firstTarget) externalOriginalBinders
        productContext <- admitCurrentOriginalProducts originalInterfaces outDir prepared
          (withWorldStageInputs prior rawProductContext)
        inventory <- maybe (fail "current original admission did not issue its inventory") pure
          (preparedCurrentOriginalInventory productContext)
        originalConstructors <- project (preparedModuleProductConstructors (preparedProductInventory productContext))
        originalSites <- project (preparedModuleProductYieldSites (preparedProductInventory productContext))
        let products = preparedProductInventory productContext
            homeProductGroups = Map.fromList
              [ (owner, Map.fromList
                  [(projectedOriginalOrdinal group, group) | group <- groups])
              | (owner, Right groups) <- preparedModuleProductOutcomes products ]
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
            originalPackageGlobals globals bindings =
              requiredOriginalExternalDemandForSeeds
                originalProducts candidates exactOriginals (Map.keysSet retainedGenerations)
                (\identity -> case Map.lookup identity (pprOriginalReferences prepared) of
                  Just _ -> Right (Just (PriorOriginalExternalDemand (Set.singleton identity) Map.empty))
                  Nothing -> stagedOriginalExternalDemand priorNative identity)
                (map globalIdentity globals ++ bindings)
        pure (originalModules,Just productContext,originalBindings,admittedOriginalBinders,
          originalConstructors,originalSites,originalPackageGlobals,homeProductGroups)
    let withOriginals context = context {projectionCurrentOriginals = originalBindings}
    finalProjections <- newIORef Map.empty
    finalDependencySelections <- newIORef Map.empty
    nativeUseObligations <- newIORef Map.empty
    nativePackageObligations <- newIORef Map.empty
    let originalReference identity = case Map.lookup identity (pprOriginalReferences prepared) of
          Just reference -> Just reference
          Nothing -> stagedOriginalReference priorNative identity
        mergePackageObligations = foldM (\known (identity,seal) -> do
          forM_ (Map.lookup identity known) $ \old -> unless (old == seal)
            (throwIO (NativePackageSealConflict identity))
          pure (Map.insert identity seal known))
        demandPackages entry packageRoots recovered = do
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
          demand <- either (ioError . userError) pure
            (originalPackageGlobals
              (candidateGlobals candidate) (candidateBindingIdentities candidate))
          let required = originalExternalPackageGlobals demand
          let uses = Set.fromList [(identity,reference) | identity <- required
                , reference <- maybe [] pure (originalReference identity)]
              packages = Set.fromList [identity | identity <- required,isNothing (originalReference identity)]
          answered <- readIORef nativeUseObligations
          let pendingUses = Set.toAscList (uses `Set.difference` Map.keysSet answered)
          unless (null pendingUses) $ do
            inputs <- maybe (throwIO WorldCellUnsupported) pure prior
            received <- exchangeWorldNativeDemand inputs (nativeDependencyQuery pendingUses)
            known <- readIORef nativePackageObligations
            combined <- foldM mergePackageObligations known (map (Map.toAscList . nativeDependencyPackages) (Map.elems received))
            writeIORef nativePackageObligations combined
            -- Acquisition is stage-wide; each final projection below selects
            -- only the entries reached from this target's actual native roots.
            modifyIORef' nativeUseObligations (Map.union received)
          acquired <- readIORef nativeUseObligations
          obligations <- foldM (\selected use -> do
            rows <- maybe (throwIO (NativeReplyCorrupt "missing acquired native use")) pure (Map.lookup use acquired)
            mergePackageObligations selected (Map.toAscList (nativeDependencyPackages rows))) Map.empty (Set.toAscList uses)
          originalImports <- Map.fromList <$> forM (Set.toAscList uses) (\use@(identity,reference) -> do
            rows <- maybe (throwIO (NativeReplyCorrupt "missing acquired native use")) pure (Map.lookup use acquired)
            pure ((reference,identity),Map.keys (nativeDependencyPackages rows)))
          let additions = Set.toAscList ((packages `Set.union` Map.keysSet obligations)
                `Set.difference` Set.fromList roots)
          if null additions then do
            final <- case intent of
              PortableExecutable -> do
                (program,constructors) <- project (finalizePreparedCandidate candidate)
                pure (PortableFinal program constructors roots)
              NativeOriginals -> do
                let isHome component = isHomeUnit (hsc_home_unit hscEnv)
                      (moduleUnit (fatOriginalOwner (preparedComponentVersion component)))
                    homeComponents = filter isHome (closureComponentSelections recovered)
                    packageComponents = filter (not . isHome) (closureComponentSelections recovered)
                    home = closureHomeModules recovered ++ concatMap preparedComponentModules homeComponents
                let homeProducts = homeProductGroups
                paired <- project (projectPreparedPackageTarget hscEnv hostBindings (T.pack target)
                  finalContext homeProducts candidates demand originalImports
                  (closureHomeModules recovered ++ concatMap preparedComponentModules homeComponents)
                  packageComponents (closureReachability recovered))
                let groups = packageTargetGroups paired
                    moduleOf identity = mkModule (stringToUnit (T.unpack (symbolUnit identity)))
                      (mkModuleName (T.unpack (symbolModule identity)))
                    packageGlobals = concatMap
                      (Execution.projectedGlobals . Execution.projectedBody . projectedGroupEncodingInput . projectedPackageEncoding) groups
                    imported = Set.fromList (map (moduleOf . Execution.globalIdentity) packageGlobals
                      ++ [moduleOf (Execution.globalIdentity global)
                         | global <- programGlobals (packageTargetProgram paired)
                         , not (isHomeUnit (hsc_home_unit hscEnv) (moduleUnit (moduleOf (Execution.globalIdentity global))))])
                unless (all (not . isHomeUnit (hsc_home_unit hscEnv) . moduleUnit . moduleOf . Execution.globalIdentity) packageGlobals)
                  (fail "package product imports a HOME compiler owner")
                defining <- foldM (\known group -> do
                  let owner = projectedPackageModule group
                      seal = projectedPackageInterfaceSha256 group
                  forM_ (Map.lookup owner known) $ \old -> unless (old == seal)
                    (fail "package products disagree on their defining interface")
                  pure (Map.insert owner seal known)) Map.empty groups
                expected <- foldM (\known (identity,seal) -> do
                  let owner = moduleOf identity
                  forM_ (Map.lookup owner known) $ \old -> unless (old == seal)
                    (throwIO (NativePackageSealConflict identity))
                  pure (Map.insert owner seal known)) defining (Map.toAscList obligations)
                captures <- capturePreparedPackageInterfaces hscEnv (compilerOwnerIface recoveryCaches)
                  [(owner,Map.lookup owner expected)
                  | owner <- Set.toAscList (Set.unions [imported,Map.keysSet defining,Map.keysSet expected])]
                  >>= either (fail . show) pure
                let recorded = Map.fromListWith Set.union
                      [((packageUnit witness,packageModule witness),Set.singleton (packageSha256 witness))
                      | evidence <- Map.elems (pprPackageImports prepared),witness <- packageInterfaces evidence]
                forM_ (Map.toAscList captures) $ \(owner,body) ->
                  forM_ (Map.lookup (unitString (moduleUnit owner),moduleNameString (moduleName owner)) recorded) $ \seals ->
                    unless (seals == Set.singleton (artifactSha256 body))
                      (fail "captured package interface differs from the actual source dependency")
                pure (NativeFinal paired captures roots)
            terminals <- nativeDependencyTerminals acquired uses
            modifyIORef' finalDependencySelections (Map.insert entry terminals)
            modifyIORef' finalProjections (Map.insert entry final)
            pure []
          else forM additions $ \identity -> do
            (identifier,_) <- resolvePackageGlobal hscEnv identity >>= either (ioError . userError) pure
            when (preparedRootIdentity identifier /= identity) $
              ioError (userError "package recovery root differs from canonical original global")
            pure identifier
    let checkDependency identity = forM_ (preparedCompilationWorldInputs completion >>= worldCellDependencyBasis) $ \basis ->
          requireUnambiguousDependencySymbols basis (Set.singleton identity)
    recover <- newPreparedRecoveryWithDemandAndReuse checkDependency (Just (pprReuseContext prepared)) (Just executor) demandPackages
      hscEnv (compilerFatIface recoveryCaches) (compilerOwnerIface recoveryCaches)
      (compilerPreparedBodies recoveryCaches) (withOriginals (contextFor firstTarget)) originalModules []
    artifacts <- forM targets $ \target -> do
      let context = withOriginals (contextFor target)
      -- The same component ledger closes ordinary dependencies and exact
      -- original-package demand before the final candidate is flattened.
      closed <- timePhase timing "prepared_recover" (recover (projectionEntry context))
      let recovered = preparedRecoveryClosure closed
      final <- readIORef finalProjections >>= maybe
        (fail "closed recovery did not issue its final projection") pure . Map.lookup (projectionEntry context)
      let (program,constructors,packageGroups,roots,packageTarget) = case final of
            PortableFinal output declarations required -> (output,declarations,[],required,Nothing)
            NativeFinal output _ required ->
              let groups = packageTargetGroups output in
              (packageTargetProgram output,packageTargetConstructors output
                ++ concatMap projectedPackageConstructors groups,groups,required,Just output)
      reportRecoveryResiduals target (closureFailures recovered)
      when (intent == PortableExecutable) $ do
        let homeUnit = T.pack (unitString (moduleUnit targetModule))
            unresolvedHome = [Execution.globalIdentity global | global <- programGlobals program
              , symbolUnit (Execution.globalIdentity global) == homeUnit]
        unless (null unresolvedHome) $
          fail ("portable output has unresolved home imports: " ++ show unresolvedHome)
      let defined = Set.union (Set.fromList (concatMap
            (Execution.projectedBinders . projectedGroupEncodingInput . projectedPackageEncoding) packageGroups)) $ Set.fromList
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
        (Tidepool.EffectSchema.mergeYieldSites
          (yieldSites ++ concatMap projectedPackageYieldSites packageGroups ++ originalSites))
      sealedSites <- forM allSites $ \site -> do
        witnesses <- forM (Tidepool.EffectSchema.ysInputTypeWitnesses site) $ \witness ->
          maybe (pure Nothing) (sealCheckedTypeWitness originalInterfaces) witness
        pure site { Tidepool.EffectSchema.ysInputTypeWitnesses = witnesses }
      terminals <- readIORef finalDependencySelections >>= maybe
        (fail "closed recovery lacks its dependency selection") pure . Map.lookup (projectionEntry context)
      pure (PreparedArtifact target program bytes (constructors ++ originalConstructors) sealedSites
        packageTarget terminals)
    finalResults <- forM targets $ \target -> do
      let entry = projectionEntry (contextFor target)
      final <- readIORef finalProjections >>= maybe
        (fail "closed recovery did not issue its final projection") pure . Map.lookup entry
      pure (target, final)
    let packageTargets = [(name, paired, additions)
          | (name, NativeFinal paired additions _) <- finalResults]
    captures <- foldM (\known (_,_,additions) -> foldM (\selected (owner,body) -> do
      forM_ (Map.lookup owner selected) $ \old -> unless (artifactSha256 old == artifactSha256 body)
        (fail "targets disagree on their actual package interface capture")
      pure (Map.insert owner body selected)) known (Map.toAscList additions)) Map.empty packageTargets
    completedContext <- case intent of
      PortableExecutable -> pure Nothing
      NativeOriginals -> do
        provenance <- preparedCompilationNativeProvenance completion
        products <- maybe (fail "native projection lacks its actual product context") pure productContext
        Just <$> retainPreparedPackageTargets provenance
          [paired | (_, paired, _) <- packageTargets] captures products
    pure (artifacts, completedContext)

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
-- Per-item facts admitted for one held-world cell. Authored identity, captures,
-- signatures, issued interface and artifact are correlated by exact identity
-- before any terminal manifest or response is exposed.
data WorldCellItemSource = WorldCellItemSource
  { worldCellItemTyped :: TypedItem
  , worldCellItemSourceIdentity :: HeldWorldCellItem
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
  , preparedWorldStageSource :: String
  , preparedWorldStageCompilerInputs :: [WorldCellInputReference]
  , preparedWorldStageExecutionInputs :: Set.Set (SymbolIdentity,Word64)
  , preparedWorldStageRuntimeReferences :: [WorldCellInputReference]
  , preparedWorldStageDependencyReferences :: [DependencyTerminal]
  , preparedWorldStageFiles :: [(FilePath,BS.ByteString)]
  , preparedWorldStageTypedPlans :: [TypedSegmentPlan]
  , preparedWorldStageReceiptFields :: Encoding
  , preparedWorldStageCompletion :: PreparedCompilation
  , preparedWorldStageProducts :: CompletedWorldProducts
  }

correlateWorldCellItems
  :: [TypedItem] -> [PreparedArtifact] -> [HeldWorldCellItem]
  -> [ResolvedTypedItemBindings] -> [(Int,[CheckedSignature])] -> IO [WorldCellItemSource]
correlateWorldCellItems typedItems artifacts sourceItems captures signatures = do
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
  sourceByKey <- uniqueEntries "world admitted execution items" heldWorldCellItemOrdinal sourceItems
  unless (Map.keysSet sourceByKey == expected)
    (fail "world admitted items do not cover exactly the typed items")
  sources <- forM typedItems $ \item -> do
    let plan = itemPlan item
        key = plannedItemOrdinal plan
    source <- maybe (fail "world typed item lost its admitted source identity") pure
      (Map.lookup key sourceByKey)
    selected <- maybe (fail "world typed item lost its captures") pure
      (Map.lookup key capturesByItem)
    itemSignatures <- maybe (fail "world typed item lost its capture witnesses") pure
      (Map.lookup key signaturesByItem)
    artifact <- maybe (fail "world typed item lost its prepared artifact") pure
      (Map.lookup (plannedItemEntry plan) artifactsByTarget)
    pure (WorldCellItemSource item source selected itemSignatures artifact)
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
      , null (cellPlanItems initial) -> writeResult (Right []) (encodeCellOut initial [] [] "") [] [] [] >> pure (Right [])
      | otherwise -> do
          let initial = heldWorldCellPlan preparation
          attempted <- trySynchronous (prepareStages preparation Nothing Set.empty [])
          case attempted of
            Right stages -> do
              let items = concatMap preparedWorldStageItems stages
                  observations = encodeCellOut initial [] (mapMaybe preparedWorldCellObservation items)
                    (concatMap preparedWorldStageSource stages)
                  common = concatMap preparedWorldStageFiles stages
                  plans = concatMap preparedWorldStageTypedPlans stages
              unless (map (\item -> let (ordinal,_,_) = preparedWorldCellItemManifest item in ordinal) items
                    == map fst (heldWorldCellSlots preparation)
                  && Map.size (Map.fromList common) == length common)
                (throwIO WorldCellCorrupt)
              writeResult (Right items) observations common plans (map encodeStageReceipt stages)
              pure (Right (map preparedWorldStageCompletion stages))
            Left failure -> case fromException failure of
              Just refusal -> rejected refusal
              Nothing -> case fromException failure of
                Just policy@(WorldModulePolicyFailure _ _) -> do
                  emitWorldAcquisitionRejection "world cell policy" (worldCellPrepareOutput question) (show policy)
                  rejected WorldCellUnsupported
                Nothing -> throwIO failure
  where
    rejected refusal = writeResult (Left refusal) BS.empty [] [] [] >> pure (Left refusal)
    prepareStages preparation prior earlierSelected reversed = do
      next <- nextStage preparation
      case next of
        Nothing -> pure (reverse reversed)
        Just stage -> do
          let initial = heldWorldCellStagePlan stage
              offset = heldWorldCellStageOffset stage
              stageDirectory = directory </> "stage-" ++ show offset
          output <- case cellPlanItems initial of
            [item] | sbKind (cellAnalysisVerdict item) == KDecl ->
              prepareDeclaration preparation prior earlierSelected stageDirectory stage
            _ -> prepareSegment preparation prior earlierSelected stageDirectory initial
          let needed = preparedWorldStageExecutionInputs output
              current = preparedWorldStageCompilerInputs output
              currentKeys = Set.fromList (map worldCellInputReferenceBinding current)
              inherited = Set.toAscList earlierSelected
          -- A stage owns its actual demand. Earlier compiler availability is
          -- insufficient: only an already selected capture can be inherited.
          _ <- either throwIO pure (selectWorldCellExecutionInputReferences needed (current ++ inherited))
          direct <- either throwIO pure (selectWorldCellExecutionInputReferences needed current)
          transitive <- either throwIO pure (selectWorldCellExecutionInputReferences
            (needed `Set.difference` currentKeys) inherited)
          let selected = Set.fromList (direct ++ transitive)
          inputs <- maybe (throwIO WorldCellCorrupt) pure
            (preparedCompilationWorldInputs (preparedWorldStageCompletion output))
          sourceDependencies <- maybe (pure []) (selectDependencyTerminals needed) (worldCellDependencyBasis inputs)
          let dependencies = Set.toAscList (Set.fromList (sourceDependencies ++ preparedWorldStageDependencyReferences output))
          unless (needed `Set.isSubsetOf` Set.fromList
              (map worldCellInputReferenceBinding (Set.toAscList selected) ++ map terminalBinding dependencies))
            (throwIO WorldCellCorrupt)
          let finalizedOutput = output {preparedWorldStageRuntimeReferences=Set.toAscList selected
                ,preparedWorldStageDependencyReferences=dependencies}
          prepareStages preparation (Just (preparedWorldStageProducts output))
            (Set.union earlierSelected selected) (finalizedOutput:reversed)
    -- Parsed demand sees the current source, but a retained Original's
    -- unfolding may reference an input selected by an earlier stage. Loan only
    -- those issued references to projection; the actual reached globals below
    -- still select the stage's runtime inputs and preserve their original facts.
    projectionInputGenerations completion earlierSelected = do
      inputs <- maybe (throwIO WorldCellCorrupt) pure (preparedCompilationWorldInputs completion)
      let references = worldCellCompilerInputReferences inputs ++ Set.toAscList earlierSelected
          bindings = Set.fromList (map worldCellInputReferenceBinding references)
      _ <- either throwIO pure (selectWorldCellExecutionInputReferences bindings references)
      sourceGenerations <- maybe (pure Map.empty) frozenDependencyGenerations (worldCellDependencyBasis inputs)
      initialGenerations <- foldM (\known (symbol,generation) -> case Map.lookup symbol known of
        Nothing -> pure (Map.insert symbol generation known)
        Just old | old == generation -> pure known
        Just _ -> throwIO WorldCellCorrupt) (worldCellRetainedGenerations inputs) (Map.toAscList sourceGenerations)
      foldM (\generations (symbol,generation) -> case Map.lookup symbol generations of
        Nothing -> pure (Map.insert symbol generation generations)
        Just existing | existing == generation -> pure generations
        Just _ -> throwIO WorldCellCorrupt) initialGenerations (Set.toAscList bindings)
    nextStage preparation = case bound of
      BoundCompilerRequest scope _ _ _ _ _ _ -> scopedPrepareWorldCellStage scope preparation
    requireStage preparation = nextStage preparation >>= maybe (throwIO WorldCellCorrupt) pure
    directory = worldCellPrepareDirectory question
    text = encodeString . T.pack
    base = rawWorldID (worldCellPrepareIdentity question)
    planDigest = SHA256.hash (worldCellPreparePlan question)
    admission = worldCellPrepareAdmission question
    refusalTag WorldCellUnsupported = 0
    refusalTag WorldCellUnavailable = 1
    refusalTag WorldCellCorrupt = 2
    encodeStageReceipt stage = encodeListLen 5 <> preparedWorldStageReceiptFields stage
      <> encodeListLen (fromIntegral (length (preparedWorldStageRuntimeReferences stage)))
      <> foldMap encodeWorldCellInputReference (preparedWorldStageRuntimeReferences stage)
      <> encodeListLen (fromIntegral (length (preparedWorldStageDependencyReferences stage)))
      <> foldMap encodeDependencyTerminal (preparedWorldStageDependencyReferences stage)
    writeResult outcome observations common plans stages = BS.writeFile (worldCellPrepareOutput question) $
      toStrictByteString (encodeListLen 9 <> text "TPCWPREP6" <> encodeBytes base
        <> encodeBytes planDigest <> encodeBytes admission
        <> (case outcome of
          Left refusal -> encodeListLen 2 <> text "rejected" <> encodeWord64 (refusalTag refusal)
          Right items -> encodeListLen 2 <> text "prepared" <> encodeListLen (fromIntegral (length items))
            <> foldMap (\(ordinal,generation,sha) -> encodeListLen 3 <> encodeWord64 ordinal
              <> encodeWord64 generation <> encodeBytes sha)
              (map preparedWorldCellItemManifest items))
        <> encodeBytes observations
        <> encodeListLen (fromIntegral (length common))
        <> foldMap (\(path,sha) -> encodeListLen 2 <> text path <> encodeBytes sha) common
        <> encodeListLen (fromIntegral (length plans)) <> foldMap encodeTypedSegmentPlan plans
        <> encodeListLen (fromIntegral (length stages)) <> mconcat stages)
    prepareSegment preparation prior earlierSelected stageDirectory initial = do
      createDirectoryIfMissing True stageDirectory
      preparedBatch <- newIORef Nothing
      attempt <- newIORef (0 :: Int)
      let compile _ = do
            issued <- requireStage preparation
            authored <- maybe (throwIO WorldCellCorrupt) pure (heldWorldCellStageTypedSource issued)
            index <- readIORef attempt
            modifyIORef' attempt (+1)
            writeIORef preparedBatch Nothing
            let stagingRoot = stageDirectory </> "typed-session-" ++ show index
                typedPlan = preparedTypedSegmentPlan authored
                prepare environment admissions typed = do
                  batch <- prepareTypedSegmentSessionBindings environment admissions typed stagingRoot
                  writeIORef preparedBatch (Just batch)
                  pure (typedSegmentSessionEnvironment batch,typedSegmentSessionGlobals batch,typedSegmentSessionInterfaces batch)
            completed <- compilePreparedWorldSegmentRequest bound
              (WithTypedSegmentPreparation prepare (PreparedSegmentProducts typedPlan Nothing))
            pure (issued,authored,completed)
      (_,(issued,authored,(completion,typed))) <- checkCellInstances compile initial
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
        inputs <- bindPreparedWorldInputs completion prior
        originalInterfaces <- newPreparedWorldInterfaceArtifacts inputs stageDirectory
        retainedGenerations <- projectionInputGenerations completion earlierSelected
        let producedGenerations = Map.fromList [(preparedRootIdentity global,generation)
              | (global,generation) <- typedSegmentSessionRetainedGlobals batch]
        unless (Map.null (Map.intersection producedGenerations retainedGenerations))
          (throwIO WorldCellCorrupt)
        let generations = Map.union producedGenerations retainedGenerations
        (artifacts,context) <- prepareArtifactsWithProjectionIntent (Just inputs) requireProjection
          originalInterfaces stageDirectory completion targets (standardAuxiliaryRoots binds)
          generations (typedSegmentSessionBindingRepresentations batch)
        correlated <- correlateWorldCellItems items artifacts (heldWorldCellStageItems issued) captures
          signaturesByItem
        products <- maybe (fail "world cell has no actual original inventory") pure context
        completedProducts <- completeWorldProducts originalInterfaces stageDirectory inputs products
        let staged = completedWorldNative completedProducts
        preparedItemsWithInputs <- forM correlated $ \correlatedItem -> do
          let item = worldCellItemTyped correlatedItem
              sourceItem = worldCellItemSourceIdentity correlatedItem
              artifact = worldCellItemArtifact correlatedItem
              selected = worldCellItemCaptures correlatedItem
              itemSignatures = worldCellItemSignatures correlatedItem
              itemPlan = typedItemPlan item
              ordinal = plannedItemOrdinal itemPlan
              generation = plannedItemGeneration itemPlan
              child = directory </> "item-" ++ show ordinal
              output = artifact {paTarget=preparedScaffoldTargetName}
              turn = TBind (map (T.pack . bbName) selected) 0 selected (paYieldSites output) (T.pack rendered)
          packageTarget <- maybe (fail "World item lost its paired package projection") pure
            (paPackageTarget output)
          createDirectoryIfMissing True child
          sidecarFiles <- writePreparedSidecars InlineYieldSites child binds Nothing (map T.pack (prWarnings result)) [output]
          programFiles <- writePreparedArtifacts child [output]
          (productFile,executionInputs) <- writeCertifiedSegmentItemProductsWithInputs
            prepared staged child packageTarget
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
                <> text (typedSegmentPlanDigest typedPlan) <> encodeBytes (heldWorldCellItemSourceDigest sourceItem)
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
          pure (PreparedWorldCellItem (fromIntegral ordinal,generation,SHA256.hash description) observation,executionInputs)
        -- Actual certification completes before exposing any private item table.
        -- Nothing here adopts a runtime value or updates an actor-current world.
        _ <- publishStagedOriginalProducts prepared staged
        let identity = encodeListLen 2 <> text "exec" <> text (typedSegmentPlanDigest typedPlan)
        finishStage stageDirectory identity (map (plannedItemOrdinal . typedItemPlan) items)
          rendered (map fst preparedItemsWithInputs) [typedPlan] completion prepared completedProducts
          (concatMap (paDependencyTerminals . worldCellItemArtifact) correlated)
          (Set.unions (map snd preparedItemsWithInputs) `Set.difference`
            Set.fromList (Map.toList producedGenerations))
    prepareDeclaration preparation prior earlierSelected stageDirectory stage = do
      let initial = heldWorldCellStagePlan stage
      (ordinal,generation,kind,sourceItem) <- case heldWorldCellStageItems stage of
        [item] -> case heldWorldCellItemSlot item of
          PlannedDeclaration generation -> pure (fromIntegral (heldWorldCellItemOrdinal item),generation,"declaration",item)
          PlannedPrologue generation -> pure (fromIntegral (heldWorldCellItemOrdinal item),generation,"prologue",item)
          _ -> throwIO WorldCellCorrupt
        _ -> throwIO WorldCellCorrupt
      let reserved = sessionModuleString (SessionModule LibMod (Generation generation))
          compile _ = do
            issued <- requireStage preparation
            original <- maybe (throwIO WorldCellCorrupt) pure (heldWorldCellStageOriginal issued)
            completion <- compilePreparedWorldRequest bound (PreparedProducts Nothing)
            pure (original,completion)
      createDirectoryIfMissing True stageDirectory
      (_,(submitted,completion)) <- checkCellInstances compile initial
      withPreparedCompilation completion $ \prepared _ _ _ -> do
        let result = pprPipelineResult prepared
        (original,inventory) <- maybe (fail "world declaration lost its actual certified stage inventory") pure
          (preparedCompilationPlannedDeclaration completion)
        unless (plannedModule original == reserved && plannedSource original == plannedSource submitted)
          (throwIO WorldCellCorrupt)
        inputs <- bindPreparedWorldInputs completion prior
        interfaces <- newPreparedWorldInterfaceArtifacts inputs stageDirectory
        generations <- projectionInputGenerations completion earlierSelected
        -- The existing original writer uses its real checking root to obtain
        -- the native inventory. No executable item or target packet is emitted.
        (artifacts,context) <- prepareArtifactsWithProjectionIntent (Just inputs) requireProjection
          interfaces stageDirectory completion [scaffoldTargetName] [] generations []
        products <- maybe (fail "world declaration has no actual original inventory") pure context
        completedProducts <- completeWorldProducts interfaces stageDirectory inputs products
        let staged = completedWorldNative completedProducts
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
        publicationInputs <- stagedDeclarationInputReferences prepared staged inventory >>= either (\reason -> do
          emitWorldAcquisitionRejection "world declaration native publication" stageDirectory reason
          throwIO WorldCellUnsupported) pure
        let identity = encodeListLen 4 <> text kind <> encodeWord64 ordinal <> text unit <> text owner
            description = toStrictByteString (encodeListLen 8 <> text "TPCWORIGINAL1"
              <> encodeBytes base <> encodeBytes planDigest <> encodeBytes admission
              <> encodeWord64 ordinal <> encodeWord64 generation
              <> encodeBytes (heldWorldCellItemSourceDigest sourceItem)
              <> encodeListLen 5 <> text kind <> text unit <> text owner
              <> encodeBytes (SHA256.hash interfaceBytes) <> text (renderPlannedDeclarationInventory inventory))
            child = directory </> "item-" ++ show ordinal
        createDirectoryIfMissing True child
        BS.writeFile (child </> "world-item.cbor") description
        let item = PreparedWorldCellItem (ordinal,generation,SHA256.hash description) Nothing
        finishStage stageDirectory identity [fromIntegral ordinal] (plannedSource original)
          [item] [] completion prepared completedProducts (concatMap paDependencyTerminals artifacts) publicationInputs
    finishStage stageDirectory identity ordinals rendered preparedItems plans completion prepared completedProducts terminals executionInputs = do
        let staged = completedWorldNative completedProducts
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
            sourceEnvelope = toStrictByteString (encodeListLen 12 <> text "TPCWSOURCE4"
              <> encodeBytes base <> encodeBytes planDigest <> encodeBytes admission
              <> identity
              <> fileRef sourceName <> fileRef (stageName "dependencies.json") <> fileRef (stageName "module-package-imports.cbor")
              <> encodeListLen (fromIntegral (length (worldCellExactImports inputFacts)))
              <> foldMap moduleRow (worldCellExactImports inputFacts)
              <> encodeSourceSelectedOriginals (worldCellSourceSelection inputFacts)
              <> encodeListLen (fromIntegral (length (worldCellSourceRootAliases inputFacts)))
              <> foldMap text (worldCellSourceRootAliases inputFacts)
              <> encodeListLen (fromIntegral (length (worldCellSourceImportRoles inputFacts)))
              <> foldMap (\(qualifier,name,boot,unit,role) -> encodeListLen 5
                <> text qualifier <> text name <> encodeBool boot <> text unit <> encodeInt role)
                  (worldCellSourceImportRoles inputFacts))
            sourceReceiptName = stageName "world-source.cbor"
        BS.writeFile (directory </> sourceReceiptName) sourceEnvelope
        let receiptRef = encodeListLen 2 <> text sourceReceiptName <> encodeBytes (SHA256.hash sourceEnvelope)
            stageReceiptFields = identity
              <> encodeListLen (fromIntegral (length ordinals)) <> foldMap (encodeWord64 . fromIntegral) ordinals
              <> encodeListLen 3 <> receiptRef
              <> fileRef (stageName "module-products.cbor") <> fileRef (stageName "certified-products.cbor")
        pure PreparedWorldCellStage
          { preparedWorldStageItems = preparedItems
          , preparedWorldStageSource = rendered
          , preparedWorldStageCompilerInputs = worldCellCompilerInputReferences inputFacts
          , preparedWorldStageExecutionInputs = executionInputs
          , preparedWorldStageRuntimeReferences = []
          , preparedWorldStageDependencyReferences = terminals
          , preparedWorldStageFiles = (sourceReceiptName,SHA256.hash sourceEnvelope):common
          , preparedWorldStageTypedPlans = plans
          , preparedWorldStageReceiptFields = stageReceiptFields
          , preparedWorldStageCompletion = completion
          , preparedWorldStageProducts = completedProducts
          }
    rawWorldID [] = BS.empty
    rawWorldID (a:b:rest) = BS.cons (fromIntegral (16 * digitToInt a + digitToInt b)) (rawWorldID rest)
    rawWorldID _ = error "world identity left its admitted shape"
