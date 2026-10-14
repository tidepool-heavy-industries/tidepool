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
  , PreparedCompilation, preparedCompilationResult, prepareCompilationOriginals
  , PreparedArtifact(..), PreparedOutputIntent(..), prepareArtifacts, prepareArtifactsWithIntent
  , prepareArtifactsWithProjection, PreviewOriginalDependenciesUnavailable, requirePreviewProjection
  ) where

import Control.Exception (Exception, bracket, evaluate, throwIO)
import Control.Monad (forM, forM_, unless, when)
import Data.IORef (newIORef, readIORef, writeIORef, modifyIORef')
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import Data.Maybe (isJust, mapMaybe)
import Data.Word (Word64)
import qualified Data.ByteString as BS
import qualified Data.Text as T
import GHC.Tc.Types (tcg_mod)
import GHC.Unit.Module (moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Types (unitString)
import System.IO (hPutStrLn, stderr)
import GHC (Ghc)
import GHC.Conc (getNumCapabilities, setNumCapabilities)
import GHC.Driver.Session (DynFlags)
import Tidepool.CellProgramState
import Tidepool.CheckedPrefixImports (CompletedValueImport)
import Tidepool.CheckedAdmission (matchesInspectionAdmission)
import Tidepool.CompilerExecution
  ( CompilerExecutionGrant, CompilerExecutor, compilerExecutionGrant
  , withCompilerExecutor, requireCompilerExecutorOpen )
import Tidepool.CompilerProducts
  ( OriginalProductWorklist, observeOriginalProjectionWithRecovery, PreparedProductContext
  , prepareOriginalProductsWithCacheAndReuse, prepareOriginalProductsWithWorklist
  , prepareCompilerProjectionContext, requireOriginalExecutableGlobals, admitCurrentOriginalProducts
  , preparedCurrentOriginalInventory, preparedProductInventory, currentOriginalBinders
  , currentOriginalBindingsExcept, currentReconciledOriginalProducts )
import Tidepool.DiagJson (InputRejection(..), SourceRejection(..))
import Tidepool.ExactHydration (ExactIfaceArtifact(..), ExactInterfaceOperations, OriginalInterfaceArtifacts)
import Tidepool.ExactScope
import Tidepool.ExecutionSchema (SymbolIdentity(..), WireProgram(..), SiteRow(..))
import qualified Tidepool.ExecutionSchema as Execution
import Tidepool.ExecutionEncode (encodeWireProgram)
import Tidepool.ExecutionProjection
  ( ProjectionContext(..), ProjectionError(..), prepareComponentProjectionWithReachability
  , projectSelectedCandidateWithHostBindings, candidateGlobals, finalizePreparedCandidate
  , preparedModuleProductConstructors, preparedModuleProductYieldSites, preparedModuleProductOutcomes
  , preparedRootIdentity )
import Tidepool.HostBindingAuthority (HostBindingRepresentation, hostBindingRepresentationJsonAuthority)
import Tidepool.ConstructorIdentity (IssuedConstructor)
import qualified Tidepool.EffectSchema
import Tidepool.CheckedCell (sealCheckedTypeWitness)
import Tidepool.CertifiedProducts (resolvePackageGlobal)
import Tidepool.ModuleCandidates (ModuleCandidate(..), CandidateGroup(..))
import Tidepool.OriginalProductRoots (requiredOriginalPackageGlobalsWithRetained, unrecoveredExactProducts)
import Tidepool.PreparedStg (PreparedModule, pmModule, pmYieldSites)
import Tidepool.PreparedRecovery
  ( RecoveryFailure, RecoveredClosure(..), newPreparedRecoveryWithDemandAndReuse
  , preparedRecoveryClosure, requirePreparedRecoveryPublication )
import Tidepool.Timing (readTimingEnabled, timePhase)
import Tidepool.ExtractRequest
import Tidepool.GhcPipeline
import Tidepool.HarnessSource (spliceHarnessProfilePragma)
import Tidepool.Session
  ( SessionScope, preparedResumeTargetName, preparedApplyEntryTargetName, preparedApplyValueTargetName )
import Tidepool.TypedSegment (TypedSegment)

-- No caller-maintained compiler record updates: argument normalization, exact
-- admission and the one executor are bound before any mode consumes the request.
data BoundCompilerRequest = BoundCompilerRequest CompilerScope WorkerRequest
  RequestOperation (Maybe ExactScope) CompilerExecutionGrant CompilerExecutor (Maybe CellCompilationInputs)

boundWorkerRequest :: BoundCompilerRequest -> WorkerRequest
boundWorkerRequest (BoundCompilerRequest _ args _ _ _ _ _) = args

boundRequestOperation :: BoundCompilerRequest -> RequestOperation
boundRequestOperation (BoundCompilerRequest _ _ operation _ _ _ _) = operation

boundRequestExactScope :: BoundCompilerRequest -> Maybe ExactScope
boundRequestExactScope (BoundCompilerRequest _ _ _ exact _ _ _) = exact

boundParserFlags :: BoundCompilerRequest -> DynFlags
boundParserFlags (BoundCompilerRequest scope _ _ _ _ _ _) = scopedParserFlags scope

withBoundCompilerRequest :: CompilerScope -> WorkerRequest -> (BoundCompilerRequest -> IO a) -> IO a
withBoundCompilerRequest compilerScope parsed use = do
  grant <- either fail pure (compilerExecutionGrant (requestCompilerJobs parsed))
  bracket getNumCapabilities setNumCapabilities $ \_previous -> do
    setNumCapabilities (requestCompilerCapabilities parsed)
    withCompilerExecutor grant $ \executor -> do
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
      use (BoundCompilerRequest compilerScope args operation exact grant executor Nothing)

-- A cell's issued lexical prefix changes only these compilation inputs. The
-- request's grant, physical search inputs and auxiliary owner remain bound.
data CellCompilationInputs = CellCompilationInputs (Maybe ProgramSourceImports)
  [((String,String),String)] [CompletedValueImport]

cellSegmentRequest :: BoundCompilerRequest -> ProgramCellState -> BoundCompilerRequest
cellSegmentRequest (BoundCompilerRequest scope args operation _ grant executor _) state =
  let exact = programExact state
      values = map exactModule (maybe [] checkedValueInterfaces (scopeCheckedCell exact))
      inputs = CellCompilationInputs (programSourceImports state) (programOriginals state)
        (selectedProgramValues (programValues state))
  in BoundCompilerRequest scope (args {requestInjectVals=values,requestRetainedGenerations=programRetained state})
    operation (Just exact) grant executor (Just inputs)

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
compileBoundCompleted (BoundCompilerRequest scope args _ exact grant executor cell) selection retained purpose session path = do
  requireCompilerExecutorOpen executor
  let effectivePurpose = maybe purpose (`cellCompilationPurpose` purpose) cell
  scopedCompileCompleted scope (WithCompilerExecution grant executor selection) retained
    (maybe effectivePurpose (ExactScopeCompile effectivePurpose) exact) session path
    (requestIncludes args) (requestBuildProductsDir args)

compileBoundRequest :: BoundCompilerRequest -> PipelineSelection result
  -> Set.Set SymbolIdentity -> CompilePurpose -> Maybe SessionScope -> FilePath -> IO result
compileBoundRequest request selection retained purpose session path =
  completedCompilerResult <$> compileBoundCompleted request selection retained purpose session path

-- The constructor stays private. Retention does not grant executor use after its
-- bracket closes; projection checks operation entry, while submissions fence a
-- concurrent close. GHC's retained mutable package cells keep their existing owner.
data PreparedCompilation = PreparedCompilation PreparedPipelineResult CompilerRecoveryCaches
  CompilerExecutor (IO ()) (Maybe OriginalProductWorklist)

preparedCompilationResult :: PreparedCompilation -> PreparedPipelineResult
preparedCompilationResult (PreparedCompilation result _ _ _ _) = result

withPreparedCompilation :: PreparedCompilation
  -> (PreparedPipelineResult -> CompilerRecoveryCaches -> CompilerExecutor -> Maybe OriginalProductWorklist -> IO a)
  -> IO a
withPreparedCompilation (PreparedCompilation result recovery executor guard worklist) use = do
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
compileWithWorklist request@(BoundCompilerRequest _ args _ _ _ executor _) selection retained purpose session path = do
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
compilePreparedRequest request@(BoundCompilerRequest _ _ _ _ _ executor _) selection retained purpose session path = do
  (completed,worklist) <- compileWithWorklist request selection retained purpose session path
  pure (PreparedCompilation (completedCompilerResult completed) (completedCompilerRecovery completed) executor (completedCompilerGuard completed) worklist)

compilePreparedSegmentRequest :: BoundCompilerRequest -> PipelineSelection PreparedSegmentProductsResult
  -> Set.Set SymbolIdentity -> CompilePurpose -> Maybe SessionScope -> FilePath -> IO (PreparedCompilation,TypedSegment)
compilePreparedSegmentRequest request@(BoundCompilerRequest _ _ _ _ _ executor _) selection retained purpose session path = do
  -- Typed segment preparation acquires its own Session bindings during the
  -- compiler pass. It has no early original observer; projection uses the
  -- captured recovery context and the existing fallback path.
  completed <- compileBoundCompleted request selection retained purpose session path
  let result = completedCompilerResult completed
  pure (PreparedCompilation (preparedSegmentProducts result) (completedCompilerRecovery completed) executor (completedCompilerGuard completed) Nothing,
    preparedSegmentCaptures result)

runBoundGhc :: BoundCompilerRequest -> Ghc a -> IO a
runBoundGhc (BoundCompilerRequest scope _ _ _ _ executor _) operation = do
  requireCompilerExecutorOpen executor
  scopedRunGhc scope operation

withBoundExactInterfaceTransaction :: BoundCompilerRequest -> (ExactInterfaceOperations -> IO a) -> IO a
withBoundExactInterfaceTransaction (BoundCompilerRequest scope args _ _ _ executor _) use = do
  requireCompilerExecutorOpen executor
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
prepareArtifacts = prepareArtifactsWithIntent NativeOriginals

data PreparedOutputIntent = NativeOriginals | PortableExecutable deriving (Eq)

prepareArtifactsWithIntent :: PreparedOutputIntent -> OriginalInterfaceArtifacts -> FilePath -> PreparedCompilation
  -> [String] -> [String] -> Map.Map SymbolIdentity Word64 -> [HostBindingRepresentation]
  -> IO ([PreparedArtifact], Maybe PreparedProductContext)
prepareArtifactsWithIntent intent = prepareArtifactsWithProjectionIntent intent requireProjection

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
        originalBindings = case intent of
          NativeOriginals -> currentOriginalBindingsExcept inventory outputRoots
          PortableExecutable -> Map.empty
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

reportRecoveryResiduals :: String -> [RecoveryFailure] -> IO ()
reportRecoveryResiduals _ [] = pure ()
reportRecoveryResiduals target failures =
  hPutStrLn stderr $ "  Prepared recovery residuals (" ++ target ++ "): "
    ++ show failures
