{-# LANGUAGE OverloadedStrings #-}

-- | Emit the compiler's complete original-product result once. Capture paths
-- belong to the supplied output directory; original source identities and
-- independently admitted candidate/exact evidence remain compiler inputs.
module Tidepool.CompilerProducts
  ( CertifiedOriginalProducts, certifiedOriginalProducts, certifiedFinalizedArtifacts
  , certifiedSourceOriginals, certifiedExecutionSource, writeCertifiedProductsKeeping, retainedOriginalInterfaces, newPreparedOriginalInterfaceArtifacts, retainProgramProducts, programLexicalRequirements, programSourceRequirements
  , certifiedRetainedOriginals, certifiedRetainedNativeVersions, PreparedProductContext, prepareOriginalProducts, prepareOriginalProductsWithExecutor
  , requireOriginalExecutableGlobals, retainPreparedPackageTargets
  , writeCertifiedProductsKeepingWithOriginals
  , CompletedWorldProducts, completeWorldProducts, completedWorldNative, completedWorldPredecessor
  , WorldStageInputs, bindWorldStageInputs, bindWorldStageInputsWithNativeDemand, requireWorldStageInputs, exchangeWorldNativeDemand, worldStagePriorOriginals, withWorldStageBindings, withPreparedWorldStageInputs, withWorldStageCompilation, withWorldStageBasis, newPreparedWorldInterfaceArtifacts
  , StagedOriginalProducts, stagedOriginalEntryOrdinal, stagedFinalizedArtifacts, stagedOriginalOutputFiles
  , StagedProductFailure(..), stagedOriginalBinders, withStagedOriginalBindings, stagedOriginalExternalDemand, stagedOriginalReference, validateStagedPriorHomeGroups, withWorldStageInputs, newPreparedOriginalInterfaceArtifactsWithPrior
  , retainTypeInterfaceCaptures, retainedTypeInterfaceBody
  , writeCertifiedSegmentProducts, writeCertifiedSegmentItemProducts, writeCertifiedSegmentItemProductsWithInputs
  , stagedDeclarationInputReferences
  , publishStagedOriginalProducts, retainStagedProgramProducts
  , prepareCompilerProjectionContext, prepareCompilerProjectionContextForEnvironment, exactProgramProductVersionFromDigest
  , OriginalProjectionCollector, newOriginalProjectionCollector, observeOriginalProjection
  , prepareOriginalProductsWithCollector
  , prepareOriginalProductsWithCache, prepareOriginalProductsWithCacheAndReuse
  , OriginalProductWorklist, observeOriginalProjectionWithRecovery, prepareOriginalProductsWithWorklist
  , CurrentOriginalInventory, admitCurrentOriginalProducts, preparedCurrentOriginalInventory
  , preparedProductInventory, currentOriginalBinders, currentOriginalBindingsExcept
  , currentReconciledOriginalProducts
  ) where

import Tidepool.RequestCompletion (finishCompletion)

import Codec.CBOR.Encoding
import Codec.CBOR.Write (toStrictByteString)
import Control.Applicative ((<|>))
import Control.Exception (Exception, throwIO, evaluate)
import Control.Concurrent.MVar (MVar)
import GHC.Types.Name.Cache (NameCache(nsNames,nsUniqChar), OrigNameCache)
import Control.Monad ((>=>), foldM, forM, forM_, unless, when)
import Data.Bits (shiftR)
import Data.ByteString qualified as BS
import Data.IORef (newIORef, readIORef, writeIORef, modifyIORef', atomicModifyIORef')
import Data.Map.Strict qualified as Map
import Data.Maybe (mapMaybe, isJust, fromMaybe, isNothing)
import Data.Set qualified as Set
import Data.Text qualified as T
import Data.Text.Encoding qualified as TE
import Data.Word (Word32, Word64)
import GHC.Tc.Types (tcg_mod)
import GHC.Types.Name (Name)
import GHC.Types.Id (isDataConWorkId_maybe, isDataConWrapId_maybe)
import GHC.Core.DataCon (dataConTyCon)
import GHC.Types.TyThing (TyThing(..), implicitTyThings)
import GHC.Types.Avail (availNames)
import GHC.Core.Class (classOpItems)
import GHC.Core.TyCon (tyConClass_maybe, isNewTyCon)
import GHC.Core.ConLike (ConLike(..))
import GHC.Core.PatSyn (patSynMatcher, patSynBuilder)
import GHC.Types.Var (Id)
import GHC.Unit.Module.ModSummary (ms_location)
import GHC.Driver.Session (importPaths)
import GHC.Driver.Env (HscEnv, hsc_dflags, hsc_NC, hsc_all_home_unit_ids, hsc_HPT, lookupType)
import GHC.Unit.Home.ModInfo (HomeModInfo(..), lookupHpt)
import GHC.Iface.Syntax (IfaceClsInst(..))
import GHC.Unit.Module (Module, ModuleName, moduleName, moduleNameString, moduleUnit, mkModuleName, mkModule)
import GHC.Unit.Module.ModIface (ModIface, mi_module, mi_iface_hash, mi_final_exts, mi_exports, mi_insts)
import GHC.Unit.Types (unitString, stringToUnit, toUnitId)
import Numeric (readHex)
import System.Directory (canonicalizePath, doesPathExist, makeAbsolute, createDirectoryIfMissing)
import System.FilePath (normalise, (</>), takeDirectory)
import System.IO (hPutStrLn, stderr)
import System.Info qualified as SystemInfo
import System.Mem.StableName (makeStableName)
import Tidepool.ArtifactBytes (ArtifactBytes, artifactBytes, artifactSha256, artifactDigestBytes)
import qualified Crypto.Hash.SHA256 as SHA256
import Tidepool.CertifiedProducts
  ( CertifiedProductKind(..), TargetCertificationContext, encodeCertifiedOriginalProductsWithPrior, certifiedContextBinders, certifiedBinderInputClosure, certifiedOriginalExternalDemand, certifiedOriginalReference, validatePriorHomeGroups
  , selectItemPackage, encodeCertifiedItemProductsWithInputs, readyNativeProductSourceSha256
  , issuePackageLinkIndex
  , PackageLinkIndex, emptyPackageLinkIndex )
import Tidepool.OriginalProductRoots
  ( ReconciledOriginalProducts, reconcileOriginalProducts, PriorHomeGroup, PriorOriginalExternalDemand )
import Tidepool.DependencyEvidence
import Tidepool.ExactHydration
  ( OriginalInterfaceArtifacts, ExactIfaceArtifact(..), sameExactInterfaceIdentity, originalInterfaceBytes, originalRetainedInterfaceCaptures, newOriginalInterfaceArtifactsWithReader, newOriginalInterfaceArtifactsWithLookup, exactInterfaceSummary )
import Tidepool.ExactScope
  ( WorldOriginalReference, WorldInputFacts, worldInputsProducer, worldInputsProfile, ExactScope , scopeProducerSha256, scopeSemanticSha256, scopeProducts, scopeRecipeOriginals, scopeExecutionOwners, scopeInterfaces, ExactCompilation(..), ExactScopeOriginal(..), ExactProduct(..), ExactOriginalGroup(..), scopeValueInterfaces
  , ExactScopeValidationReason(..), revalidateExactScopesAtWithOutputs
  , FreshOutputSeals, freshOutputSealsFromWrites, appendFreshOutputSeals, freshOutputSealFacts
  , ExactCompilationPublication(..), writeCheckedExactCompilationWithOutputsAndPublication
  , writeRetainedExactCompilationWithOutputsAndPublication
  , scopeInterfaceToken, scopeOriginalBytes
  , canonicalProofInterfaceBody, canonicalProofOriginalBytes, relocateCanonicalInterfaceProof
  , CanonicalInterfaceProof, CanonicalInterfaceAdmission(..), captureCanonicalMetadata, canonicalOrigin, isSourceOriginal
  , captureFinalizedSourceOriginals, originalGroupFromProjected, originalGroupFromCandidate
  , extendSourceSelectedOriginals, extendExactScopeGeneration, extendExactExecutionSources, extendExactExecutionSourcesWithinBudget
  , scopeLexical, scopeInterfaceEvidence, scopeExecutionGraphs, ExactInterfaceEvidence(..), canonicalCertificateSha256, canonicalCertificateBytes, canonicalCertificatePath, canonicalSourceSha256 )
import Tidepool.ExecutionEncode
  ( ModuleProductEncoding, moduleProductInput, moduleProductBytes
  , prepareModuleProductEncodingFromGroups, encodeModuleProductInventory, encodePackageGroup
  , projectedGroupEncodingInput )
import Tidepool.ExecutionProjection
  ( ProjectionContext(..), ProjectionError(..), PreparedModuleProducts, OriginalGroupOmission(..)
  , preparedModuleProductEncodings, preparedModuleProductOmissions, resolveTextPackageUnit
  , rawOriginalProductOwner
  , OriginalProjectionCache, newOriginalProjectionCache, projectCachedOriginalHomeModuleProducts
  , lookupCachedOriginalHomeModuleProducts
  , rawOriginalProductDemands
  , rawProjectionObservation, observeRawOriginalGroupWork
  , settleOriginalHomeModuleProducts, settleOriginalHomeModuleProductsWithoutOwners
  , RawModuleProducts, preparedTopIdentityBindings, preparedRootIdentity
  , PreparedPackageTarget
  , packageTargetName, packageTargetGroups, packageTargetImports, packageTargetHomeImports, packageTargetPriorHomeImports, packageTargetOriginalImports
  , projectedPackageModule
  , projectedPackageInterfaceSha256, projectedPackageDefiningVersion, projectedPackageEncoding )
import Tidepool.PlannedDeclaration
  ( PlannedDeclarationInventory, plannedOriginalOwner, plannedInterfaceFingerprint )
import Tidepool.ExecutionSchema
  ( Architecture(..), Endianness(..), SymbolIdentity(..), TargetDescriptor(..), WireProgram
  , GlobalDecl(..), ProjectedGroup(..), ProjectedGroupBody(..), PackageCodeOwner(..) )
import qualified Tidepool.ExecutionSchema as Execution
import Tidepool.ExecutionSource
  ( WorkerExecutionSource(..), SourceRecipeUnavailable(..), ExecutionSourceRecipe(..)
  , ExecutionSourceGraph(..), executionGraphBytes, executionGraphSha256, ExecutionSourceIdentity(..), ExecutionSourceOwner(..)
  , ExecutionSourceFailure(..), executionIdentityKey, issueExecutionSourceRecipe
  , executionSourceInheritedOwners, ExecutionSourceRef(..), executionSourceProspectiveReferences )
import Tidepool.ExtractUtil (shaHex)
import Tidepool.FinalizedModuleArtifacts
  ( FinalizedModuleArtifacts, captureFinalizedModuleArtifacts, captureFinalizedModuleTypeInterfaces, finalizedTypeInterfaces
  , materializeFinalizedModuleArtifactsWithOutputs, LocalFinalizedAdmission
  , finalizedLocalAdmissions, localFinalizedCore, localFinalizedInterface, localFinalizedSourceSha256
  , localFinalizedPackageBody, localFinalizedPackageEvidence
  )
import Tidepool.GhcPipeline
  ( NativeProductProvenance, nativeProductProducer, nativeProductProfile
  , WorldCellRefusal(..), CompiledWorldStage, CompilerRecoveryCaches, WorldCompilerCompletion, withCompiledWorldStage, withCompiledWorldStageRecovery, withCompiledWorldInputs, withCompiledWorldNativeCoordinates, inheritCompiledWorldCanonicalRecovery, requireSameCompiledWorldStage, finishCompiledWorldStage, worldCompilerCompletionInputs, requireWorldProductInputs
  , PreparedPipelineResult, pprPipelineResult, pprModules, pprProductInterfaces, pprFinalizedModules
  , pprCanonicalOriginalInputs, pprAcquiredCanonicalOriginals, pprPackageImports, pprCandidateAdmissions, pprOriginalBindings, pprOriginalReferences, pprAcceptedCandidates, prHscEnv, prCanonicalInterfaceAdmissions
  , prInjectedSessionInterfaces, prProducedSessionInterfaces, prTargetTcGblEnv, PreparedModuleObserver(..)
  , PreparedModuleCompletionInputs(..), PreparedSourceRoute(..), preparedSourceRoute
  , acquireWorldSourceOriginal, worldSourceOriginalFinalization, worldSourceOriginalPackages, captureWorldSourceOriginals
  , preparedFreshDependencies, preparedExactCompilation
  , preparedHomeRequirements, preparedCandidateOriginal, preparedCandidateProof
  , revalidatePreparedCandidateInputs )
import Tidepool.GhcPipeline (WorldCellInputs, worldCellTypeInterfaces)
import Tidepool.ModuleCandidates (ModuleCandidate(..), candidateExecutionSources)
import Tidepool.PackageWitness (PackageImportEvidence(..), PackageImportRoot(..))
import Tidepool.PreparedFormatting (resolveFormattingAuthority)
import Tidepool.PreparedJson (JsonAuthority, resolveJsonAuthorityWithCanonicalInterfaces)
import Tidepool.PreparedTime (resolveTimeAuthority)
import Tidepool.WorkerServer (NativeDemandExchange, NativeDemandReply, requireNativeDemandOwner, exchangeNativeDemand)
import Tidepool.PreparedStg
  ( PreparedModule, pmModule, pmSitedSiblings, runPreparedModuleTask
  , PreparedBodyCache, PreparedBodyReuse(..), newPreparedBodyCache, newPreparedOriginalModuleTaskPreparer, newPreparedCanonicalModuleTaskPreparerWithLookup, acquirePreparedModule )
import Tidepool.CompilerExecution (CompilerExecutor, withCompilerExecutor, serialCompilerExecutionGrant, runCompilerTasks, runCompilerTasksWithAttempt)
import Tidepool.HomeProducts
  ( CanonicalOriginalInput, CanonicalOriginalInputs, validateCanonicalOriginalInputs, canonicalOriginalInputAdditions, canonicalOriginalVersion, lookupCanonicalOriginalInput
  , AdmittedFinalizedOriginal, admittedOriginalModule, admitCompiledCanonicalOriginal, CandidateCoreFailure(..)
  , admittedOriginalProof, admittedOriginalInterface, originalVersionLookup, originalVersionSeal )
import Tidepool.FinalizedModule (finalizedHomeModInfo)
import Tidepool.Timing
  ( readTimingEnabled, timePhase, timeDetailPhase, timeTaskDetailPhase, TaskOwner(..), TaskComponent(..), emitCount
  , ReuseContext, ReuseModule(..), ReuseStage(..), ReuseDecision(..), ReuseReason(..)
  , ReuseVersionKind(..), emitReuse, emitReuseComplete
  , ProjectionObservation, OriginalGroupWork(..), emitOriginalGroupWork )

-- The production worker and original-product fixtures share the compiler
-- profile and native package/type authorities. Callers select only their real
-- root/target and independently admitted host inputs.
prepareCompilerProjectionContext
  :: PreparedPipelineResult -> Map.Map SymbolIdentity Word64 -> Module -> String -> [String]
  -> Maybe JsonAuthority -> IO ProjectionContext
prepareCompilerProjectionContext prepared retainedGenerations owner target auxiliaryRoots hostJsonAuthority = do
  context <- prepareCompilerProjectionContextForEnvironment
    (prHscEnv (pprPipelineResult prepared))
    (prCanonicalInterfaceAdmissions (pprPipelineResult prepared))
    retainedGenerations owner target auxiliaryRoots hostJsonAuthority
  pure context {projectionCurrentOriginals = pprOriginalBindings prepared}

prepareCompilerProjectionContextForEnvironment
  :: HscEnv -> Map.Map (String,String) CanonicalInterfaceAdmission -> Map.Map SymbolIdentity Word64 -> Module -> String -> [String]
  -> Maybe JsonAuthority -> IO ProjectionContext
prepareCompilerProjectionContextForEnvironment environment canonicalInterfaces retainedGenerations owner target auxiliaryRoots hostJsonAuthority = do
  timing <- readTimingEnabled
  let symbol occurrence = SymbolIdentity (T.pack (unitString (moduleUnit owner)))
        (T.pack (moduleNameString (moduleName owner))) "value" (T.pack occurrence) Nothing
  formattingAuthority <- timePhase timing "formatting_authority" $ resolveFormattingAuthority environment
  timeAuthority <- timePhase timing "time_authority" $ resolveTimeAuthority environment
  jsonAuthority <- timePhase timing "json_authority" $ case hostJsonAuthority of
    Just authority -> pure (Just authority)
    Nothing -> resolveJsonAuthorityWithCanonicalInterfaces environment
      canonicalInterfaces
  textAuthority <- timePhase timing "text_authority" $ resolveTextPackageUnit environment
  (architecture, abi) <- case SystemInfo.arch of
    "x86_64" -> pure (X86_64, "sysv64")
    "aarch64" -> pure (Aarch64, "aapcs64")
    other -> ioError (userError ("prepared execution is not configured for " ++ other))
  pure ProjectionContext
    { projectionProfile = "ghc-9.12-prepared-stg"
    , projectionToolchain = "ghc-9.12.2"
    , projectionTarget = TargetDescriptor architecture LittleEndian 64 64 abi []
    , projectionRetainedGenerations = retainedGenerations
    , projectionCurrentOriginals = Map.empty
    , projectionEntry = symbol target
    , projectionAuxiliaryRoots = map symbol auxiliaryRoots
    , projectionFormattingAuthority = formattingAuthority
    , projectionTimeAuthority = timeAuthority
    , projectionJsonAuthority = jsonAuthority
    , projectionTextUnit = textAuthority
    }

-- Completed native tasks retain their raw projection under the exact compiler
-- object and projection inputs. A differently prepared body or authority falls
-- back to local projection; an owner name alone never supplies cached results.
type OriginalProjectionCollector = OriginalProjectionCache

newOriginalProjectionCollector :: IO OriginalProjectionCollector
newOriginalProjectionCollector = newOriginalProjectionCache

-- The pipeline acquires authorities on its coordinator before tasks start.
-- The returned observer runs inside each admitted native task, then publishes
-- immutable results under a short lock; it does no live Session acquisition.
observeOriginalProjection :: OriginalProjectionCollector
  -> Map.Map SymbolIdentity Word64 -> [String] -> Maybe JsonAuthority
  -> HscEnv -> Map.Map ModuleName ModIface -> Module -> Maybe ExactScope
  -> PreparedModuleCompletionInputs -> IO PreparedModuleObserver
observeOriginalProjection completed retained auxiliaryRoots json
    environment interfaces owner _exact inputs = do
  baseContext <- prepareCompilerProjectionContextForEnvironment environment (completionCanonicalInterfaces inputs) retained owner
    "__original_projection" auxiliaryRoots json
  let context = baseContext {projectionCurrentOriginals = completionExternalOriginalBindings inputs}
      observe prepared = do
        _ <- projectCachedOriginalHomeModuleProducts completed environment interfaces context prepared
        pure ()
  pure (PreparedModuleObserver observe (\_ -> pure ()))

data CertifiedOriginalProducts = CertifiedOriginalProducts
  { certifiedOriginalProducts :: [ModuleProductEncoding]
  , certifiedSourceCaptures :: Map.Map (String,String) SourceOriginalCapture
  , certifiedFinalizedArtifacts :: FinalizedModuleArtifacts
  , certifiedExecutionSource :: WorkerExecutionSource
  , certifiedRetainedOriginals :: Map.Map (String,String) CanonicalInterfaceProof
  , certifiedRetainedNativeVersions :: Map.Map (String,String) String
  , certifiedTargetContext :: Maybe TargetCertificationContext
  , certifiedNameBindings :: !CertifiedNameBindings
  -- Final output readiness belongs to this certification, not the earlier
  -- interface-only prepared capture. Every publication issues these same facts.
  , certifiedFreshDependencies :: DependencyEvidence
  }

-- The canonical certificate and exact local compiler-object capture are one
-- source-availability owner. Neither claims that native groups were prepared.
data SourceOriginalCapture = SourceOriginalCapture !CanonicalInterfaceProof !LocalFinalizedAdmission

-- Names are valid in their actual compiler NameCache. No Id/details knot or
-- environment is retained by this small native-availability projection.
data StagedProductFailure
  = StagedNameCacheMismatch
  | StagedWorldMismatch
  | CertifiedNameConflict SymbolIdentity SymbolIdentity
  deriving Show
instance Exception StagedProductFailure

data CertifiedNameBindings = CertifiedNameBindings !(Char,MVar OrigNameCache) !(Map.Map Name SymbolIdentity)

nameCacheIdentity :: HscEnv -> (Char,MVar OrigNameCache)
nameCacheIdentity environment = let names = hsc_NC environment in (nsUniqChar names,nsNames names)

mergeOriginalBindings :: Map.Map Name SymbolIdentity -> Map.Map Name SymbolIdentity
  -> IO (Map.Map Name SymbolIdentity)
mergeOriginalBindings initial additions = foldM insertBinding initial (Map.toAscList additions)
  where
    insertBinding known (name,identity) = case Map.lookup name known of
      Just previous | previous /= identity -> throwIO (CertifiedNameConflict previous identity)
      _ -> pure (Map.insert name identity known)


certifiedSourceOriginals :: CertifiedOriginalProducts -> Map.Map (String,String) CanonicalInterfaceProof
certifiedSourceOriginals = Map.map (\(SourceOriginalCapture proof _) -> proof) . certifiedSourceCaptures

-- Only the writer below pairs certification with its actual compiled stage.
-- The completed value contains no phase environment or continuation.
data CompletedWorldProducts = CompletedWorldProducts !WorldCompilerCompletion !StagedOriginalProducts !(Maybe CompletedWorldProducts)

completedWorldNative :: CompletedWorldProducts -> StagedOriginalProducts
completedWorldNative (CompletedWorldProducts _ native _) = native

completedWorldPredecessor :: CompletedWorldProducts -> WorldCompilerCompletion
completedWorldPredecessor (CompletedWorldProducts completion _ _) = completion

completeWorldProducts :: OriginalInterfaceArtifacts -> FilePath -> WorldStageInputs
  -> PreparedProductContext -> IO CompletedWorldProducts
completeWorldProducts interfaces directory inputs@(WorldStageInputs stage prior _ _ _ _ _) products =
  withPreparedWorldStageInputs inputs $ \prepared _ _ -> do
    case preparedStageInputs products of
      Just (WorldStageInputs issued _ _ _ _ _ _) -> requireSameCompiledWorldStage stage issued
      Nothing -> fail "World certification lacks its bound stage inputs"
    staged <- writeCertifiedSegmentProducts
      (importPaths (hsc_dflags (prHscEnv (pprPipelineResult prepared)))) interfaces directory prepared products
    let versions = [version | owner <- Map.keys (preparedRetainedOriginals products)
          , Just input <- [lookupStageCanonical inputs owner]
          , Just version <- [canonicalOriginalVersion input]]
    completed <- finishCompiledWorldStage stage versions
    pure (CompletedWorldProducts completed staged prior)

-- Segment facts remain in memory until the segment's terminal proof succeeds.
-- Item products may borrow the target context without publishing this authority.
data StagedOriginalProducts = StagedOriginalProducts
  CertifiedOriginalProducts FilePath BS.ByteString FreshOutputSeals !StageInputAdditions

-- Compiler availability survives a stage without implying body demand. Native
-- and local-source certificates stay in the same staged product above.
data StageInputAdditions = StageInputAdditions
  !(Map.Map (String,String) (ExactIfaceArtifact,ArtifactBytes)) !(Maybe CanonicalOriginalInputs)

-- One live view binds current compilation and the actual immediate predecessor.
-- Base acquisitions are borrowed; completed stages own only their additions.
data WorldStageInputs = WorldStageInputs !CompiledWorldStage !(Maybe CompletedWorldProducts)
  !(Map.Map Name SymbolIdentity)
  !(Map.Map (String,String) (ExactIfaceArtifact,ArtifactBytes))
  !(Maybe CanonicalOriginalInputs) !(Map.Map Module AdmittedFinalizedOriginal) !(Maybe NativeDemandExchange)

bindWorldStageInputs :: CompiledWorldStage -> Maybe CompletedWorldProducts -> IO WorldStageInputs
bindWorldStageInputs stage prior = bindWorldStageInputsWithNativeDemand stage prior Nothing

requireWorldStageInputs :: CompiledWorldStage -> WorldStageInputs -> IO ()
requireWorldStageInputs issued (WorldStageInputs stage _ _ _ _ _ _) =
  requireSameCompiledWorldStage issued stage

bindWorldStageInputsWithNativeDemand :: CompiledWorldStage -> Maybe CompletedWorldProducts
  -> Maybe NativeDemandExchange -> IO WorldStageInputs
bindWorldStageInputsWithNativeDemand stage prior native = withCompiledWorldStage stage $ \prepared completion -> do
  forM_ native $ \exchange -> withCompiledWorldNativeCoordinates stage $ \admission world inputs _ ->
    requireNativeDemandOwner exchange admission world inputs
  requireWorldProductInputs prepared (completedWorldPredecessor <$> prior)
  names <- maybe (pure Map.empty) (stagedOriginalBindings prepared . completedWorldNative) prior
  let current = pprCanonicalOriginalInputs prepared
      inputs = worldCompilerCompletionInputs completion
      captures = Map.fromList [((exactUnit artifact,exactModule artifact),(artifact,body))
        | (artifact,body) <- worldCellTypeInterfaces inputs]
  validateCanonicalOriginalInputs (lookupCompletedCanonical prior) current
  forM_ (Map.toAscList captures) $ \(key,pair) ->
    forM_ (lookupCompletedType prior (pairOwner pair)) $ \old ->
      either fail (const (pure ())) (retainTypeInterfaceCaptures (Map.singleton key old) (Map.singleton key pair))
  pure (WorldStageInputs stage prior names captures current (pprAcquiredCanonicalOriginals prepared) native)

exchangeWorldNativeDemand :: WorldStageInputs -> BS.ByteString -> IO NativeDemandReply
exchangeWorldNativeDemand (WorldStageInputs stage _ _ _ _ _ native) payload =
  withCompiledWorldNativeCoordinates stage $ \_ _ _ ordinal -> case native of
    Nothing -> throwIO WorldCellUnsupported
    Just exchange -> exchangeNativeDemand exchange ordinal payload

pairOwner :: (ExactIfaceArtifact,ArtifactBytes) -> Module
pairOwner (artifact,_) = mkModule (stringToUnit (exactUnit artifact)) (mkModuleName (exactModule artifact))

worldStagePriorOriginals :: WorldStageInputs -> Maybe StagedOriginalProducts
worldStagePriorOriginals (WorldStageInputs _ prior _ _ _ _ _) = completedWorldNative <$> prior

withPreparedWorldStageInputs :: WorldStageInputs
  -> (PreparedPipelineResult -> WorldCellInputs -> Maybe StagedOriginalProducts -> IO a) -> IO a
withPreparedWorldStageInputs inputs@(WorldStageInputs stage _ _ _ _ _ _) use =
  withCompiledWorldStage stage $ \prepared completion ->
    use prepared (worldCompilerCompletionInputs completion) (worldStagePriorOriginals inputs)

withWorldStageCompilation :: WorldStageInputs
  -> (PreparedPipelineResult -> CompilerRecoveryCaches -> CompilerExecutor -> IO a) -> IO a
withWorldStageCompilation (WorldStageInputs stage _ _ _ _ _ _) = withCompiledWorldStageRecovery stage

withWorldStageBasis :: WorldStageInputs -> (WorldInputFacts -> IO a) -> IO a
withWorldStageBasis (WorldStageInputs stage _ _ _ _ _ _) = withCompiledWorldInputs stage

withWorldStageBindings :: WorldStageInputs -> ProjectionContext -> IO ProjectionContext
withWorldStageBindings (WorldStageInputs stage _ names _ _ _ _) context = withCompiledWorldStage stage $ \_ _ -> do
  selected <- mergeOriginalBindings (projectionCurrentOriginals context) names
  pure context {projectionCurrentOriginals=selected}

lookupCompletedType :: Maybe CompletedWorldProducts -> Module -> Maybe (ExactIfaceArtifact,ArtifactBytes)
lookupCompletedType Nothing _ = Nothing
lookupCompletedType (Just (CompletedWorldProducts _ staged previous)) owner =
  Map.lookup (unitString (moduleUnit owner),moduleNameString (moduleName owner)) (stagedTypeInterfaces staged)
    <|> lookupCompletedType previous owner

lookupCompletedCanonical :: Maybe CompletedWorldProducts -> Module -> Maybe CanonicalOriginalInput
lookupCompletedCanonical Nothing _ = Nothing
lookupCompletedCanonical (Just (CompletedWorldProducts _ (StagedOriginalProducts _ _ _ _ (StageInputAdditions _ current)) previous)) owner =
  (current >>= (\inputs -> lookupCanonicalOriginalInput inputs owner)) <|> lookupCompletedCanonical previous owner

lookupStageType :: WorldStageInputs -> Module -> Maybe (ExactIfaceArtifact,ArtifactBytes)
lookupStageType (WorldStageInputs _ prior _ current _ _ _) owner =
  Map.lookup (unitString (moduleUnit owner),moduleNameString (moduleName owner)) current <|> lookupCompletedType prior owner

lookupStageCanonical :: WorldStageInputs -> Module -> Maybe CanonicalOriginalInput
lookupStageCanonical (WorldStageInputs _ prior _ _ current _ _) owner =
  (current >>= (\inputs -> lookupCanonicalOriginalInput inputs owner)) <|> lookupCompletedCanonical prior owner

stagedOriginalEntryOrdinal :: StagedOriginalProducts -> SymbolIdentity -> IO Word32
stagedOriginalEntryOrdinal (StagedOriginalProducts certified _ _ _ _) entry = case
  [Execution.projectedOriginalOrdinal group | original <- certifiedOriginalProducts certified
    , let (unit,owner,_,groups) = moduleProductInput original
    , unit == symbolUnit entry, owner == symbolModule entry
    , group <- groups, entry `elem` Execution.projectedBinders group] of
    [ordinal] -> pure ordinal
    _ -> fail "typed item entry has no unique compiler-issued original group"

stagedFinalizedArtifacts :: StagedOriginalProducts -> FinalizedModuleArtifacts
stagedFinalizedArtifacts (StagedOriginalProducts certified _ _ _ _) = certifiedFinalizedArtifacts certified

stagedOriginalBinders :: StagedOriginalProducts -> Set.Set SymbolIdentity
stagedOriginalBinders (StagedOriginalProducts certified _ _ _ _) =
  maybe Set.empty certifiedContextBinders (certifiedTargetContext certified)

-- The staged owner supplies both the names and their exact native certificate.
-- Availability is not runtime demand and includes the preceding output roots.
stagedOriginalBindings :: PreparedPipelineResult -> StagedOriginalProducts -> IO (Map.Map Name SymbolIdentity)
stagedOriginalBindings prepared (StagedOriginalProducts certified _ _ _ _) = do
  let CertifiedNameBindings names bindings = certifiedNameBindings certified
  unless (names == nameCacheIdentity (prHscEnv (pprPipelineResult prepared)))
    (throwIO StagedNameCacheMismatch)
  unless (case (preparedSourceRoute prepared,certifiedExecutionSource certified) of
      (PreparedWorldSource base domain,WorldExecutionSourceUnavailable oldBase oldDomain _) ->
        base == oldBase && domain == oldDomain
      _ -> False) (throwIO StagedWorldMismatch)
  pure bindings

validateStagedOriginalProducts :: PreparedPipelineResult -> StagedOriginalProducts -> IO ()
validateStagedOriginalProducts prepared staged = do
  _ <- stagedOriginalBindings prepared staged
  pure ()

withStagedOriginalBindings :: PreparedPipelineResult -> Maybe StagedOriginalProducts
  -> ProjectionContext -> IO ProjectionContext
withStagedOriginalBindings _ Nothing context = pure context
withStagedOriginalBindings prepared (Just prior) context = do
  names <- stagedOriginalBindings prepared prior
  selected <- mergeOriginalBindings (projectionCurrentOriginals context) names
  pure context {projectionCurrentOriginals=selected}

stagedOriginalReference :: Maybe StagedOriginalProducts -> SymbolIdentity -> Maybe WorldOriginalReference
stagedOriginalReference prior identity = do
  StagedOriginalProducts certified _ _ _ _ <- prior
  context <- certifiedTargetContext certified
  certifiedOriginalReference context identity

stagedOriginalExternalDemand :: Maybe StagedOriginalProducts -> SymbolIdentity
  -> Either String (Maybe PriorOriginalExternalDemand)
stagedOriginalExternalDemand Nothing _ = Right Nothing
stagedOriginalExternalDemand (Just (StagedOriginalProducts certified _ _ _ _)) identity = do
  context <- maybe (Left "prior staged originals lack their certified native context") Right
    (certifiedTargetContext certified)
  certifiedOriginalExternalDemand context identity

validateStagedPriorHomeGroups :: Maybe StagedOriginalProducts
  -> Map.Map PriorHomeGroup [SymbolIdentity] -> Either String ()
validateStagedPriorHomeGroups prior reached
  | Map.null reached = Right ()
  | otherwise = do
      staged <- maybe (Left "prior HOME package roots lack their completed predecessor") Right prior
      let StagedOriginalProducts previous _ _ _ _ = staged
      previousContext <- maybe (Left "prior HOME package roots lack their certification context") Right
        (certifiedTargetContext previous)
      validatePriorHomeGroups previousContext reached

-- File identities come from actual emitter writes, including the staged
-- terminal certificate. Callers do not inventory an output directory.
stagedOriginalOutputFiles :: StagedOriginalProducts -> [(FilePath,String)]
stagedOriginalOutputFiles (StagedOriginalProducts _ certificate bytes outputs _) =
  (certificate,shaHex bytes) : freshOutputSealFacts outputs

-- This index retains type bytes, not native group or original authority.
-- Those remain in the same stage's certified TargetCertificationContext.
stagedTypeInterfaces :: StagedOriginalProducts -> Map.Map (String,String) (ExactIfaceArtifact,ArtifactBytes)
stagedTypeInterfaces (StagedOriginalProducts _ _ _ _ (StageInputAdditions interfaces _)) = interfaces

-- A relocated view may borrow the already-issued type capture. Preserve its
-- descriptor and compare semantic requirements and owned bytes, rather than
-- treating the new packet's output directory as a new implementation.
retainTypeInterfaceCaptures
  :: Map.Map (String,String) (ExactIfaceArtifact,ArtifactBytes)
  -> Map.Map (String,String) (ExactIfaceArtifact,ArtifactBytes)
  -> Either String (Map.Map (String,String) (ExactIfaceArtifact,ArtifactBytes))
retainTypeInterfaceCaptures previous additions = do
  forM_ (Map.toAscList additions) $ \(key,(artifact,body)) -> do
    unless (key == (exactUnit artifact,exactModule artifact)
        && exactSha256 artifact == artifactSha256 body)
      (Left "type interface capture differs from its owner or body seal")
    forM_ (Map.lookup key previous) $ \(old,oldBody) ->
      unless (sameExactInterfaceIdentity old artifact
          && oldBody == body)
        (Left "type interface capture conflicts with its retained owner")
  pure (Map.union previous additions)

retainedTypeInterfaceBody
  :: Map.Map (String,String) (ExactIfaceArtifact,ArtifactBytes)
  -> ExactIfaceArtifact -> Maybe ArtifactBytes
retainedTypeInterfaceBody retained artifact = do
  (selected,body) <- Map.lookup (exactUnit artifact,exactModule artifact) retained
  if sameExactInterfaceIdentity selected artifact
      && artifactSha256 body == exactSha256 artifact
    then Just body else Nothing

data PreparedProductContext = PreparedProductContext
  { preparedProductInventory :: PreparedModuleProducts
  , preparedRetainedOriginals :: Map.Map Module AdmittedFinalizedOriginal
  , preparedProductModules :: [PreparedModule]
  , preparedRawProducts :: Maybe [RawModuleProducts]
  , preparedExternalBinders :: Set.Set SymbolIdentity
  , preparedCurrentOriginalInventory :: Maybe CurrentOriginalInventory
  , preparedStageInputs :: Maybe WorldStageInputs
  , preparedPackageOutputs :: !PreparedPackageOutputs
  }

data PreparedPackageOutputs = PreparedPackageOutputs
  ![(FilePath,BS.ByteString)] !(Map.Map (T.Text,T.Text) (FilePath,ArtifactBytes))
  !PackageLinkIndex

emptyPreparedPackageOutputs :: PreparedPackageOutputs
emptyPreparedPackageOutputs = PreparedPackageOutputs [] Map.empty emptyPackageLinkIndex

-- The active stage supplies deployment provenance; the paired projection and
-- its captured interfaces supply code and byte custody. The writer only copies
-- these immutable inputs, never resolves another package path.
retainPreparedPackageTargets :: NativeProductProvenance -> [PreparedPackageTarget]
  -> Map.Map Module ArtifactBytes -> PreparedProductContext -> IO PreparedProductContext
retainPreparedPackageTargets provenance targets captures context = do
  forM_ (preparedStageInputs context) $ \inputs -> withWorldStageBasis inputs $ \basis ->
    unless (nativeProductProducer provenance == worldInputsProducer basis
      && nativeProductProfile provenance == worldInputsProfile basis)
      (throwIO WorldCellCorrupt)
  do
    let producer = nativeProductProducer provenance
        profile = nativeProductProfile provenance
        targetGroups = [ (packageTargetName target, group)
                       | target <- targets
                       , group <- packageTargetGroups target ]
    unless (BS.length producer == 32 && BS.length profile == 32)
      (fail "package product deployment commitments are malformed")
    unless (length targets == Set.size (Set.fromList (map packageTargetName targets)))
      (fail "package projections repeat a target")
    encodings <- forM targetGroups $ \(name, group) -> do
      let owner = projectedPackageModule group
      body <- maybe (fail "package product lacks its captured defining interface") pure
        (Map.lookup owner captures)
      unless (artifactDigestBytes body == projectedPackageInterfaceSha256 group)
        (fail "package product differs from its captured defining interface")
      let provenance' = PackageCodeOwner
            (T.pack (unitString (moduleUnit owner))) (T.pack (moduleNameString (moduleName owner)))
            producer profile (projectedPackageInterfaceSha256 group) (projectedPackageDefiningVersion group)
          bytes = encodePackageGroup provenance' (projectedPackageEncoding group)
      pure (name, group, bytes)
    ownersByTarget <- Map.fromList <$> forM targets (\target -> do
      owners <- foldM insertOwner Map.empty
                [ (binder, fromIntegral index)
                | (index, (name, group, _)) <- zip [0 :: Int ..] encodings
                , name == packageTargetName target
                , binder <- projectedBinders
                    (projectedGroupEncodingInput (projectedPackageEncoding group)) ]
      pure (packageTargetName target,owners))
    occurrenceFacts <- forM encodings $ \(name, group, bytes) -> do
      imports <- forM
        [ globalIdentity global
        | global <- projectedGlobals (projectedBody
            (projectedGroupEncodingInput (projectedPackageEncoding group))) ]
        (resolveOwner ownersByTarget name)
      pure
        ( SHA256.hash bytes
        , T.pack (unitString (moduleUnit (projectedPackageModule group)))
        , T.pack (moduleNameString (moduleName (projectedPackageModule group)))
        , projectedPackageInterfaceSha256 group
        , projectedPackageDefiningVersion group
        , imports )
    targetRoots <- Map.fromList <$> forM targets (\target -> do
      roots <- mapM (resolveOwner ownersByTarget (packageTargetName target))
        (packageTargetImports target)
      pure (packageTargetName target, roots))
    priorHomeGroups <- Map.fromList <$> forM targets (\target -> do
      let reached = packageTargetPriorHomeImports target
      either fail pure (validateStagedPriorHomeGroups (preparedPriorOriginalProducts context) reached)
      pure (packageTargetName target,reached))
    let homeRows =
          [ ((packageTargetName target, unit, name, ordinal), identities)
          | target <- targets
          , (unit,name,ordinal, identities) <- packageTargetHomeImports target ]
          ++ [ ((target,unit,name,ordinal),identities)
             | (target,reached) <- Map.toAscList priorHomeGroups
             , ((unit,name,_,ordinal),identities) <- Map.toAscList reached ]
    homeRoots <- foldM (insertHomeRoot ownersByTarget) Map.empty homeRows
    let expectedHomeRoots = Set.fromList (map fst homeRows)
    unless (Map.keysSet homeRoots == expectedHomeRoots)
      (fail "fresh HOME package roots are incomplete or crossed")
    originalRoots <- Map.fromList . concat <$> forM targets (\target ->
      forM (Map.toAscList (packageTargetOriginalImports target)) $ \((reference,binder), identities) -> do
        unless (identities == Set.toAscList (Set.fromList identities))
          (fail "Original use package identities repeat or are unordered")
        links <- forM identities $ \identity -> do
          occurrence <- resolveOwner ownersByTarget (packageTargetName target) identity
          pure (identity,occurrence)
        pure ((packageTargetName target,reference,binder),links))
    let packageWrites = Map.toAscList (Map.fromList
          [("package-products" </> shaHex bytes ++ ".cbor",bytes)
          | (_,_,bytes) <- encodings])
        products = [(path, SHA256.hash bytes) | (path,bytes) <- packageWrites]
        interfaces = Map.fromList
          [((T.pack (unitString (moduleUnit owner)),T.pack (moduleNameString (moduleName owner))),
            ("package-interfaces" </> artifactSha256 body ++ ".hi",body))
          | (owner,body) <- Map.toAscList captures]
        writes = packageWrites
    links <- either fail pure
      (issuePackageLinkIndex occurrenceFacts ownersByTarget targetRoots
        homeRoots expectedHomeRoots priorHomeGroups originalRoots products)
    pure context {preparedPackageOutputs=PreparedPackageOutputs writes interfaces links}
  where
    insertOwner owners (binder,occurrence)
      | Map.member binder owners = fail ("package target repeats an exported binder: " ++ show binder)
      | otherwise = pure (Map.insert binder occurrence owners)

    insertHomeRoot owners known (key, identities) = do
      target <- case key of
        (target,_,_,_) -> pure target
      links <- mapM (resolveOwner owners target) identities
      if Map.member key known
        then fail ("fresh HOME package roots repeat " ++ show key)
        else pure (Map.insert key links known)

    resolveOwner owners target identity = case Map.lookup identity
        (Map.findWithDefault Map.empty target owners) of
      Just occurrence -> pure occurrence
      Nothing -> fail ("target " ++ T.unpack target
        ++ " package import lacks its issued occurrence: " ++ show identity)

withWorldStageInputs :: Maybe WorldStageInputs -> PreparedProductContext -> PreparedProductContext
withWorldStageInputs inputs context = context {preparedStageInputs=inputs}

preparedPriorOriginalProducts :: PreparedProductContext -> Maybe StagedOriginalProducts
preparedPriorOriginalProducts context = preparedStageInputs context >>= worldStagePriorOriginals

-- This owner issues the batch only after original Core, exact interface bytes,
-- and package witnesses have passed the publication checks. The writer consumes
-- these same immutable encodings rather than making a second availability choice.
data ObservedOriginalProduct = ObservedOriginalProduct
  ModuleProductEncoding (Maybe ProjectionObservation)

originalProductEncoding :: ObservedOriginalProduct -> ModuleProductEncoding
originalProductEncoding (ObservedOriginalProduct encoding _) = encoding

data CurrentOriginalInventory = CurrentOriginalInventory
  { currentOriginalAvailability :: Map.Map (String,String) ProductAvailability
  , currentOriginalProducts :: [ObservedOriginalProduct]
  , currentOriginalPackages :: Map.Map (String,String) BS.ByteString
  , currentOriginalFinalized :: FinalizedModuleArtifacts
    -- Kept with these immutable products through item output selection; both
    -- package demand and certification consume this same reconciliation.
  , currentReconciledOriginalProducts :: ReconciledOriginalProducts
  , currentOriginalNames :: Map.Map Name SymbolIdentity
    -- Already admitted native originals are imports, never fresh emitted groups.
  , currentOriginalExternalNames :: Map.Map Name SymbolIdentity
  }

currentOriginalBinders :: CurrentOriginalInventory -> Set.Set SymbolIdentity
currentOriginalBinders inventory = Set.fromList (Map.elems (currentOriginalExternalNames inventory))
  `Set.union` Set.fromList
  [binder | product' <- currentOriginalProducts inventory
    , let (_,_,_,groups) = moduleProductInput (originalProductEncoding product')
    , group <- groups, binder <- projectedBinders group]

-- Entries and auxiliary roots retain their complete original recursive group.
-- Every remaining group crosses the executable boundary through its exact Name.
currentOriginalBindingsExcept :: CurrentOriginalInventory -> Set.Set SymbolIdentity
  -> Map.Map Name SymbolIdentity
currentOriginalBindingsExcept inventory roots = Map.union (currentOriginalExternalNames inventory)
  (Map.filter (`Set.member` imported) (currentOriginalNames inventory))
  where
    imported = Set.fromList
      [binder | product' <- currentOriginalProducts inventory
        , let (_,_,_,groups) = moduleProductInput (originalProductEncoding product')
        , group <- groups
        , Set.null (Set.fromList (projectedBinders group) `Set.intersection` roots)
        , binder <- projectedBinders group]

admitCurrentOriginalProducts :: OriginalInterfaceArtifacts -> FilePath
  -> PreparedPipelineResult -> PreparedProductContext -> IO PreparedProductContext
admitCurrentOriginalProducts originalInterfaces outDir prepared productContext =
  case preparedCurrentOriginalInventory productContext of
    Just _ -> pure productContext
    Nothing -> do
      timing <- readTimingEnabled
      let environment = prHscEnv (pprPipelineResult prepared)
          freshOwners = freshSourceOwners (preparedFreshDependencies prepared)
          selectedContext = productContext
      finalized <- timeDetailPhase timing "module_products" "capture_finalization" $
        capturePreparedFinalization originalInterfaces outDir prepared freshOwners
      let withheld = Set.fromList
            [owner | prepared' <- preparedProductModules selectedContext
              , let owner = pmModule prepared'
                    key = (unitString (moduleUnit owner),moduleNameString (moduleName owner))
              , Map.notMember owner (preparedRetainedOriginals selectedContext)
              , maybe False (not . isJust . localFinalizedCore)
                  (Map.lookup key (finalizedLocalAdmissions finalized))]
          admittedContext = case preparedRawProducts selectedContext of
            Nothing -> selectedContext
            Just raw | not (Set.null withheld) -> selectedContext {preparedProductInventory = fst
              (settleOriginalHomeModuleProductsWithoutOwners environment
                (preparedExternalBinders selectedContext) withheld raw)}
            Just _ -> selectedContext
      (availability,products,packages) <- admitModuleProducts originalInterfaces admittedContext
        (finalizedLocalAdmissions finalized) (pprProductInterfaces prepared) (pprPackageImports prepared)
      reconciled <- either (ioError . userError) pure (reconcileOriginalProducts
        (compilationScope <$> preparedExactCompilation prepared)
        [(T.unpack unit, T.unpack name, map originalGroupFromProjected groups)
          | product <- products, let (unit,name,_,groups) = moduleProductInput (originalProductEncoding product)])
      priorNames <- maybe (pure Map.empty) (stagedOriginalBindings prepared)
        (preparedPriorOriginalProducts productContext)
      externalNames <- mergeOriginalBindings priorNames (pprOriginalBindings prepared)
      let inventory = CurrentOriginalInventory
            { currentOriginalAvailability = availability
            , currentOriginalProducts = products
            , currentOriginalPackages = packages
            , currentOriginalFinalized = finalized
            , currentReconciledOriginalProducts = reconciled
            , currentOriginalNames = preparedTopIdentityBindings (preparedProductModules productContext)
            , currentOriginalExternalNames = Map.filter (`Set.member` preparedExternalBinders productContext)
                externalNames
            }
      pure admittedContext {preparedCurrentOriginalInventory = Just inventory}

capturePreparedFinalization :: OriginalInterfaceArtifacts -> FilePath
  -> PreparedPipelineResult -> Set.Set (String,String) -> IO FinalizedModuleArtifacts
capturePreparedFinalization originals directory prepared freshOwners = do
  let environment = prHscEnv (pprPipelineResult prepared)
      allFinalized = pprFinalizedModules prepared
      evidence = preparedFreshDependencies prepared
      fresh = Map.filter (\original ->
        let owner = mi_module (hm_iface (finalizedHomeModInfo original))
            (unit,name) = (unitString (moduleUnit owner),moduleNameString (moduleName owner))
        in (unit,name) `Set.member` freshOwners) allFinalized
  captured <- captureFinalizedModuleArtifacts originals environment fresh
    (pprPackageImports prepared) evidence directory
  captureFinalizedModuleTypeInterfaces originals environment (Map.difference allFinalized fresh) captured

-- Source qualification precedes native product availability. In particular,
-- an actual fresh module begins as ProductInterfaceOnly until this publisher
-- admits its original groups; ProductReady cannot be a source-role premise.
freshSourceOwners :: DependencyEvidence -> Set.Set (String,String)
freshSourceOwners evidence = Set.fromList
  [ (dependencyModuleUnit node,dependencyModuleName node)
  | node <- dependencyModules evidence
  , not (dependencyModuleBoot node)
  , let path = dependencyModuleSource node
  , not (null path), path `Set.member` paths ]
  where
    paths = Set.fromList (map dependencySourcePath (dependencySources evidence))


-- Final emitted home imports need a native original group. A retained
-- generation already supplies its implementation; a canonical interface
-- alone does not. Check after recovery's package-root fixed point, when the
-- selected executable globals are known, rather than rejecting unused owners.
requireOriginalExecutableGlobals
  :: HscEnv -> Set.Set SymbolIdentity -> [GlobalDecl]
  -> Either ProjectionError ()
requireOriginalExecutableGlobals env available globals =
  if Set.null unavailable then Right ()
    else Left (UnavailableOriginalHomeDependencies (Set.toAscList unavailable))
  where
    unavailable = Set.fromList
      [identity | global <- globals, globalRequiredGeneration global == Nothing
        , let identity = globalIdentity global
        , toUnitId (stringToUnit (T.unpack (symbolUnit identity)))
            `Set.member` hsc_all_home_unit_ids env
        , identity `Set.notMember` available]

-- Close emitted original home globals before adjudicating unavailable owners.
-- Each defining owner is prepared once from its admitted complete Core pair.
prepareOriginalProducts
  :: HscEnv -> Maybe ExactScope -> Map.Map ModuleName ModIface -> ProjectionContext
  -> Set.Set SymbolIdentity -> [PreparedModule]
  -> IO ([PreparedModule], PreparedProductContext)
prepareOriginalProducts env exact interfaces context external initial =
  (withCompilerExecutor (const (pure ())) serialCompilerExecutionGrant >=> finishCompletion) $ \executor ->
    prepareOriginalProductsWithExecutor executor env exact interfaces context external initial

prepareOriginalProductsWithExecutor
  :: CompilerExecutor -> HscEnv -> Maybe ExactScope -> Map.Map ModuleName ModIface
  -> ProjectionContext -> Set.Set SymbolIdentity -> [PreparedModule]
  -> IO ([PreparedModule], PreparedProductContext)
prepareOriginalProductsWithExecutor = prepareOriginalProductsUsingCollector Nothing Nothing

prepareOriginalProductsWithCollector
  :: OriginalProjectionCollector -> CompilerExecutor -> HscEnv -> Maybe ExactScope
  -> Map.Map ModuleName ModIface -> ProjectionContext -> Set.Set SymbolIdentity -> [PreparedModule]
  -> IO ([PreparedModule], PreparedProductContext)
prepareOriginalProductsWithCollector collector = prepareOriginalProductsUsingCollector (Just collector) Nothing

-- Production compilation supplies its completed universe's body cache. The
-- serial adapters above keep an isolated cache for standalone fixture calls.
prepareOriginalProductsWithCache
  :: PreparedBodyCache -> Maybe OriginalProjectionCollector -> CompilerExecutor
  -> HscEnv -> Maybe ExactScope -> Map.Map ModuleName ModIface -> ProjectionContext
  -> Set.Set SymbolIdentity -> [PreparedModule] -> IO ([PreparedModule],PreparedProductContext)
prepareOriginalProductsWithCache cache collector executor = prepareOriginalProductsWithCacheAndReuse Nothing cache collector executor Nothing

-- The fallback retains the issuing compile cycle. Its worklist finish owns
-- the body/recovery/projection terminals, just as an acquired observer does.
prepareOriginalProductsWithCacheAndReuse
  :: Maybe ReuseContext -> PreparedBodyCache -> Maybe OriginalProjectionCollector -> CompilerExecutor
  -> Maybe WorldStageInputs -> HscEnv -> Maybe ExactScope -> Map.Map ModuleName ModIface -> ProjectionContext
  -> Set.Set SymbolIdentity -> [PreparedModule] -> IO ([PreparedModule],PreparedProductContext)
prepareOriginalProductsWithCacheAndReuse reuse cache collector executor sourceInputs =
  prepareOriginalProductsUsingCollectorWithReuse collector (Just cache) executor reuse sourceInputs
    Nothing
    (case sourceInputs of Just (WorldStageInputs _ _ _ _ _ acquired _) -> acquired; Nothing -> Map.empty)

-- The coordinator owns incorporation and demand discovery. Workers publish
-- only immutable raw leaves through the existing projection cache. This driver
-- lives for one compiler result; completed body/raw nodes live in its universe.
data OriginalProductWorklist = OriginalProductWorklist
  { worklistObserve :: PreparedModule -> IO ()
  , worklistComplete :: PreparedModule -> IO ()
  , worklistFinish :: IO ([PreparedModule],PreparedProductContext)
  , worklistMatches :: HscEnv -> Maybe ExactScope -> Map.Map ModuleName ModIface -> ProjectionContext
      -> Set.Set SymbolIdentity -> [PreparedModule] -> IO Bool
  , worklistFallback :: HscEnv -> Maybe ExactScope -> Map.Map ModuleName ModIface
      -> ProjectionContext -> Set.Set SymbolIdentity -> [PreparedModule]
      -> IO ([PreparedModule],PreparedProductContext)
  }

observeOriginalProjectionWithRecovery :: OriginalProjectionCollector -> PreparedBodyCache
  -> CompilerExecutor -> Map.Map SymbolIdentity Word64 -> [String] -> Maybe JsonAuthority
  -> HscEnv -> Map.Map ModuleName ModIface -> Module -> Maybe ExactScope
  -> PreparedModuleCompletionInputs -> IO (PreparedModuleObserver,OriginalProductWorklist)
observeOriginalProjectionWithRecovery collector cache executor retained auxiliaryRoots json
    env interfaces owner exact inputs = do
  baseContext <- prepareCompilerProjectionContextForEnvironment env (completionCanonicalInterfaces inputs) retained owner
    "__original_projection" auxiliaryRoots json
  let context = baseContext {projectionCurrentOriginals = completionExternalOriginalBindings inputs}
  worklist <- newOriginalProductWorklist collector cache executor (Just (completionReuseContext inputs)) env exact interfaces context
    (completionExternalOriginals inputs) (completionSourceOwners inputs) (completionSiblings inputs) Nothing
    (completionCanonicalOriginalInputs inputs) (completionAcquiredCanonicalOriginals inputs)
  pure (PreparedModuleObserver (worklistObserve worklist) (worklistComplete worklist),worklist)

-- A later target may add relevant generations or host authority. Adoption
-- requires the same physical source modules and their relevant raw inputs;
-- otherwise completed immutable cache nodes seed a fresh exact worklist.
prepareOriginalProductsWithWorklist :: OriginalProductWorklist
  -> HscEnv -> Maybe ExactScope -> Map.Map ModuleName ModIface -> ProjectionContext
  -> Set.Set SymbolIdentity -> [PreparedModule] -> IO ([PreparedModule],PreparedProductContext)
prepareOriginalProductsWithWorklist worklist env exact interfaces context external initial = do
  accepted <- worklistMatches worklist env exact interfaces context external initial
  if accepted then worklistFinish worklist
    else worklistFallback worklist env exact interfaces context external initial

prepareOriginalProductsUsingCollector
  :: Maybe OriginalProjectionCollector -> Maybe PreparedBodyCache -> CompilerExecutor -> HscEnv -> Maybe ExactScope
  -> Map.Map ModuleName ModIface -> ProjectionContext -> Set.Set SymbolIdentity -> [PreparedModule]
  -> IO ([PreparedModule], PreparedProductContext)
prepareOriginalProductsUsingCollector collector stable executor =
  prepareOriginalProductsUsingCollectorWithReuse collector stable executor Nothing Nothing Nothing Map.empty

prepareOriginalProductsUsingCollectorWithReuse
  :: Maybe OriginalProjectionCollector -> Maybe PreparedBodyCache -> CompilerExecutor
  -> Maybe ReuseContext -> Maybe WorldStageInputs
  -> Maybe CanonicalOriginalInputs -> Map.Map Module AdmittedFinalizedOriginal
  -> HscEnv -> Maybe ExactScope -> Map.Map ModuleName ModIface
  -> ProjectionContext -> Set.Set SymbolIdentity -> [PreparedModule]
  -> IO ([PreparedModule],PreparedProductContext)
prepareOriginalProductsUsingCollectorWithReuse collector stable executor reuse sourceInputs canonicalInputs acquired env exact interfaces context external initial = do
  cache <- maybe newPreparedBodyCache pure stable
  completedRaw <- maybe newOriginalProjectionCache pure collector
  worklist <- newOriginalProductWorklist completedRaw cache executor reuse env exact interfaces context external
    (Set.fromList (map pmModule initial)) (Map.unions (map pmSitedSiblings initial)) sourceInputs canonicalInputs acquired
  _ <- runCompilerTasks executor
    (\prepared -> worklistObserve worklist prepared >> pure prepared)
    (\_ prepared -> worklistComplete worklist prepared) initial
  worklistFinish worklist

newOriginalProductWorklist :: OriginalProjectionCollector -> PreparedBodyCache -> CompilerExecutor
  -> Maybe ReuseContext -> HscEnv -> Maybe ExactScope -> Map.Map ModuleName ModIface -> ProjectionContext
  -> Set.Set SymbolIdentity -> Set.Set Module -> Map.Map String Id
  -> Maybe WorldStageInputs
  -> Maybe CanonicalOriginalInputs -> Map.Map Module AdmittedFinalizedOriginal -> IO OriginalProductWorklist
newOriginalProductWorklist completedRaw cache executor reuse env exact interfaces context external sourceOwners frozenSiblings sourceInputs canonicalInputs acquired = do
  timing <- readTimingEnabled
  workRef <- newIORef (0 :: Integer,0 :: Integer)
  reuseRef <- newIORef (0 :: Integer,0 :: Integer)
  reportedRef <- newIORef False
  originalPreparerRef <- newIORef Nothing
  modulesRef <- newIORef Map.empty
  admittedRef <- newIORef acquired
  attemptedRef <- newIORef Set.empty
  rawRef <- newIORef Map.empty
  knownRef <- newIORef external
  let lookupCanonical owner = (canonicalInputs >>= (\inputs -> lookupCanonicalOriginalInput inputs owner))
        <|> (sourceInputs >>= (\inputs -> lookupStageCanonical inputs owner))
  canonicalPreparer <- newPreparedCanonicalModuleTaskPreparerWithLookup env cache lookupCanonical
  let canonicalVersion = maybe (const Nothing) originalVersionLookup exact
      ownerVersion selected owner =
        let unit = unitString (moduleUnit owner)
            name = moduleNameString (moduleName owner)
        in case canonicalVersion owner of
          Just version -> Just (ReuseModule unit name CanonicalSeal (originalVersionSeal version))
          Nothing -> case Map.lookup (moduleName owner) selected of
            Just iface | mi_module iface == owner ->
              Just (ReuseModule unit name InterfaceFingerprint (show (mi_iface_hash (mi_final_exts iface))))
            _ -> Nothing
      report stage decision reason owner bytes = forM_ reuse $ \identity ->
        emitReuse timing identity stage decision reason owner 1 bytes
      acquireOriginal siblings owner producer missing = case lookupCanonical owner of
        Just input -> do
          forM_ sourceInputs $ \(WorldStageInputs stage _ _ _ _ _ _) ->
            forM_ (canonicalOriginalVersion input) (inheritCompiledWorldCanonicalRecovery stage)
          canonicalPreparer siblings owner
        Nothing -> acquirePrior siblings owner producer missing
      acquirePrior siblings owner producer missing = do
        local <- case sourceInputs of
          Nothing -> pure Nothing
          Just inputs -> withPreparedWorldStageInputs inputs $ \prepared _ prior -> case prior of
            Nothing -> pure Nothing
            Just (StagedOriginalProducts certified _ _ _ _) -> case Map.lookup
                (unitString (moduleUnit owner),moduleNameString (moduleName owner))
                (certifiedSourceCaptures certified) of
              Nothing -> pure Nothing
              Just (SourceOriginalCapture proof capture) -> do
                acquired <- acquireWorldSourceOriginal prepared owner capture
                traverse (\original -> do
                  captured <- captureCanonicalMetadata Nothing proof
                  admitCompiledCanonicalOriginal env captured
                    (worldSourceOriginalFinalization original) (worldSourceOriginalPackages original)) acquired
        case local of
          Nothing -> acquireCanonical siblings owner producer missing
          Just original -> do
            let (artifact,_,_) = admittedOriginalInterface original
            task <- acquirePreparedModule env (ms_location (exactInterfaceSummary env artifact))
              siblings (admittedOriginalModule original)
            emitCount timing "world.cell.prior_source_preparations" 1
            pure (Just (original,PreparedBodyMiss,task))
      acquireCanonical siblings owner producer missing = case exact of
        Nothing -> pure Nothing
        Just scope -> do
          when (any (\original -> (originalUnit original,originalModule original) ==
              (unitString (moduleUnit owner),moduleNameString (moduleName owner))) (scopeProducts scope))
            (fail ("required binder is absent from the admitted original native census: "
              ++ show (unitString (moduleUnit owner),moduleNameString (moduleName owner))
              ++ "; producing module: "
              ++ show (unitString (moduleUnit producer),moduleNameString (moduleName producer))
              ++ "; missing binders: " ++ show (Set.toAscList (Set.filter (\identity ->
                symbolUnit identity == T.pack (unitString (moduleUnit owner))
                  && symbolModule identity == T.pack (moduleNameString (moduleName owner))) missing))))
          existing <- readIORef originalPreparerRef
          prepare <- case existing of
            Just ready -> pure ready
            Nothing -> do
              ready <- newPreparedOriginalModuleTaskPreparer env cache scope
              writeIORef originalPreparerRef (Just ready)
              -- Scope admission is real work, shared by every later owner in
              -- this worklist. Already advertised native originals need none.
              report OriginalRecovery ReuseWork Recovery Nothing Nothing
              pure ready
          prepare siblings owner
      lower selected prepared = do
        let version = ownerVersion selected (pmModule prepared)
            versionStamp = case version of
              Just (ReuseModule _ _ _ stamp) -> Just stamp
              _ -> Nothing
        (hit,raw) <- timeTaskDetailPhase timing "prepared_graph" "raw_projection_task_service" reuse Nothing
          (TaskOwner (pmModule prepared) versionStamp Nothing WholeModuleTask) $
          projectCachedOriginalHomeModuleProducts completedRaw env selected context prepared
        report RawProjection (if hit then ReuseHit else ReuseMiss)
          (if hit then Matched else Recovery) version (if hit then Just 0 else Nothing)
        observeRawOriginalGroupWork timing reuse
          (if hit then RawProjectionReused else RawProjectionAcquired) raw
        unless hit (report RawProjection ReuseWork Recovery version Nothing)
        atomicModifyIORef' workRef (\(projected,hits) ->
          (if hit then (projected,hits + 1) else (projected + 1,hits),()))
        pure raw
      completed prepared raw = do
        modifyIORef' modulesRef (Map.insert (pmModule prepared) prepared)
        modifyIORef' knownRef (`Set.union` Set.fromList
          (Map.elems (preparedTopIdentityBindings [prepared])))
        modifyIORef' rawRef (Map.insert (rawOriginalProductOwner raw) raw)
        modules <- readIORef modulesRef
        attempted <- readIORef attemptedRef
        known <- readIORef knownRef
        let missing = rawOriginalProductDemands raw `Set.difference` known
            pending = Set.toAscList (Set.fromList
              [owner | identity <- Set.toAscList missing
                , let owner = mkModule
                        (stringToUnit (T.unpack (symbolUnit identity)))
                        (mkModuleName (T.unpack (symbolModule identity)))
                , toUnitId (moduleUnit owner) `Set.member` hsc_all_home_unit_ids env
                , owner `Set.notMember` sourceOwners
                , Map.notMember owner modules, owner `Set.notMember` attempted])
        modifyIORef' attemptedRef (`Set.union` Set.fromList pending)
        let siblings = Map.unions (frozenSiblings : map pmSitedSiblings (Map.elems modules))
        acquired <- fmap (Map.fromList . mapMaybe id) $ forM pending $ \owner -> do
          original <- acquireOriginal siblings owner (rawOriginalProductOwner raw) missing
          let version = ownerVersion interfaces owner
          -- A present canonical owner checks the current artifact bytes. A
          -- prepared hit avoids decoding and lowering, not validation work.
          case original of
            Nothing -> report OriginalRecovery ReuseMiss Absent version (Just 0)
            Just (_,observation,_) -> do
              let hit = observation == PreparedBodyReused
                  disabled = observation == PreparedBodyDisabled
              report OriginalRecovery ReuseWork Recovery version Nothing
              -- A prepared hit also retained its decoded canonical original.
              -- A prepared miss alone says nothing about decoded-Core reuse.
              when (hit || disabled) (report OriginalRecovery ReuseHit Matched version Nothing)
              report PreparedBody (case observation of
                  PreparedBodyReused -> ReuseHit
                  PreparedBodyDisabled -> ReuseDisabled
                  PreparedBodyMiss -> ReuseMiss)
                (case observation of
                  PreparedBodyReused -> Matched
                  PreparedBodyDisabled -> CacheDisabled
                  PreparedBodyMiss -> Recovery) version (if hit then Just 0 else Nothing)
          pure (fmap (\value -> (owner,value)) original)
        admitted <- readIORef admittedRef
        let originals = Map.map (\(original,_,_) -> original) acquired
            allAdmitted = Map.union admitted originals
            selected = Map.union interfaces (Map.fromList
              [(moduleName owner,hm_iface (finalizedHomeModInfo (admittedOriginalModule original)))
                | (owner,original) <- Map.toAscList allAdmitted])
        modifyIORef' admittedRef (Map.union originals)
        let tasks = [(owner,task) | (owner,(_,_,task)) <- Map.toAscList acquired]
        modifyIORef' reuseRef (\(hits,lowered) ->
          (hits + fromIntegral (length [() | (_,PreparedBodyReused,_) <- Map.elems acquired]),
           lowered + fromIntegral (length [() | (_,observation,_) <- Map.elems acquired, observation /= PreparedBodyReused])))
        _ <- runCompilerTasksWithAttempt executor
          (\attempt (owner,task) -> do
            let versionStamp = case ownerVersion selected owner of
                  Just (ReuseModule _ _ _ stamp) -> Just stamp
                  _ -> Nothing
            original <- timeTaskDetailPhase timing "prepared_graph" "prepared_stg_task_service"
              reuse (Just attempt) (TaskOwner owner versionStamp Nothing WholeModuleTask)
              (runPreparedModuleTask task)
            case Map.lookup owner acquired of
              Just (_,observation,_) | observation /= PreparedBodyReused ->
                report PreparedBody ReuseWork
                  (if observation == PreparedBodyDisabled then CacheDisabled else Recovery)
                  (ownerVersion selected owner) Nothing
              _ -> pure ()
            product' <- lower selected original
            pure (original,product'))
          (\_ (original,product') -> completed original product') tasks
        pure ()
      observe prepared = lower interfaces prepared >> pure ()
      complete prepared = do
        captured <- lookupCachedOriginalHomeModuleProducts completedRaw env interfaces context prepared
        raw <- maybe (fail "native completion has no worker-published original projection") pure captured
        completed prepared raw
      finish = do
        modules <- Map.elems <$> readIORef modulesRef
        unless (sourceOwners `Set.isSubsetOf` Set.fromList (map pmModule modules))
          (fail "original product settlement still has queued source owners")
        admitted <- readIORef admittedRef
        raw <- Map.elems <$> readIORef rawRef
        let (products,_) = settleOriginalHomeModuleProducts env external raw
        (projected,hits) <- readIORef workRef
        emitCount timing "original_raw_projected_modules" projected
        emitCount timing "original_raw_cache_hits" hits
        (reused,lowered) <- readIORef reuseRef
        emitCount timing "original_prepared_cache_hits" reused
        emitCount timing "original_prepared_new_modules" lowered
        emitCount timing "original_advertised_native_binders" (fromIntegral (Set.size external))
        reported <- atomicModifyIORef' reportedRef (\previous -> (True,previous))
        unless reported $ forM_ reuse $ \identity ->
          forM_ [OriginalRecovery,PreparedBody,RawProjection] (emitReuseComplete timing identity)
        pure (modules,PreparedProductContext products admitted modules (Just raw) external Nothing Nothing emptyPreparedPackageOutputs)
      samePhysical left right = do
        first <- evaluate left >>= makeStableName
        second <- evaluate right >>= makeStableName
        pure (first == second)
      matches selectedEnv selectedExact selectedInterfaces selectedContext selectedExternal initial = do
        recorded <- readIORef modulesRef
        originals <- readIORef rawRef
        admitted <- readIORef admittedRef
        let owners = Set.fromList (map pmModule initial)
            selectedVersion = maybe (const Nothing) originalVersionLookup selectedExact
            sameVersion owner = case (canonicalVersion owner,selectedVersion owner) of
              (Just previous,Just current) -> previous == current
              _ -> False
            selected = Map.union selectedInterfaces (Map.fromList
              [(moduleName owner,hm_iface (finalizedHomeModInfo (admittedOriginalModule original)))
                | (owner,original) <- Map.toAscList admitted])
        if selectedExternal /= external || owners /= sourceOwners
            || not (all sameVersion (Map.keys admitted)) then pure False else do
          sameSource <- fmap and $ forM initial $ \prepared ->
            maybe (pure False) (samePhysical prepared) (Map.lookup (pmModule prepared) recorded)
          sameRaw <- fmap and $ forM (Map.elems recorded) $ \prepared ->
            case Map.lookup (pmModule prepared) originals of
              Nothing -> pure False
              Just oldRaw -> do
                selectedRaw <- lookupCachedOriginalHomeModuleProducts completedRaw selectedEnv
                  selected selectedContext prepared
                maybe (pure False) (`samePhysical` oldRaw) selectedRaw
          pure (sameSource && sameRaw)
      fallback = prepareOriginalProductsUsingCollectorWithReuse
        (Just completedRaw) (Just cache) executor reuse sourceInputs canonicalInputs acquired
  pure (OriginalProductWorklist observe complete finish matches fallback)

-- Captures come from the exact scope, including its admitted checked values,
-- or the candidate owner. Their original bytes supply type dependency seals;
-- this lookup does not turn checked values into source finalizations or native
-- execution recipes.
retainedOriginalInterfaces :: PreparedPipelineResult -> [ExactIfaceArtifact]
retainedOriginalInterfaces prepared =
  [artifact | scope <- maybe [] pure (compilationScope <$> preparedExactCompilation prepared)
    , (artifact, _, _) <- scopeInterfaces scope]
  ++ [artifact | scope <- maybe [] pure (compilationScope <$> preparedExactCompilation prepared)
    , artifact <- scopeValueInterfaces scope]
  ++ [ExactIfaceArtifact (candidateUnit candidate) (candidateModule candidate)
        (candidateInterface candidate) (candidateInterfaceSha256 candidate)
        (candidateInterfaceRequirements candidate)
     | candidate <- pprAcceptedCandidates prepared]

-- | Share the selected interfaces with finalization and type-witness sealing.
newPreparedOriginalInterfaceArtifacts :: PreparedPipelineResult -> FilePath -> IO OriginalInterfaceArtifacts
newPreparedOriginalInterfaceArtifacts prepared = newPreparedOriginalInterfaceArtifactsWithPrior prepared Nothing Nothing

newPreparedOriginalInterfaceArtifactsWithPrior
  :: PreparedPipelineResult -> Maybe WorldCellInputs -> Maybe StagedOriginalProducts
  -> FilePath -> IO OriginalInterfaceArtifacts
newPreparedOriginalInterfaceArtifactsWithPrior prepared worldInputs prior directory = do
  let worldInterfaces = maybe [] worldCellTypeInterfaces worldInputs
      previous = maybe Map.empty stagedTypeInterfaces prior
      current = Map.fromList [((exactUnit artifact,exactModule artifact),(artifact,body)) | (artifact,body) <- worldInterfaces]
  unless (all (\(artifact,body) -> Map.lookup (exactUnit artifact,exactModule artifact) current == Just (artifact,body)) worldInterfaces)
    (fail "current type interface captures conflict")
  retained <- either fail pure (retainTypeInterfaceCaptures previous current)
  let readRetained artifact = case retainedTypeInterfaceBody retained artifact of
        Just body -> pure body
        _ -> readOrdinary artifact
  newOriginalInterfaceArtifactsWithReader readRetained (prHscEnv result)
    (pprFinalizedModules prepared) (retainedOriginalInterfaces prepared ++ map fst (Map.elems retained))
    (prInjectedSessionInterfaces result) (prProducedSessionInterfaces result) directory
  where
    result = pprPipelineResult prepared
    readOrdinary artifact = case compilationScope <$> preparedExactCompilation prepared of
      Just scope | artifact `elem` ([iface | (iface,_,_) <- scopeInterfaces scope] ++ scopeValueInterfaces scope) ->
        scopeInterfaceToken scope artifact
      _ -> case [preparedCandidateProof admission | admission <- pprCandidateAdmissions prepared
          , let candidate = preparedCandidateOriginal admission
          , (candidateUnit candidate,candidateModule candidate,candidateInterface candidate)
              == (exactUnit artifact,exactModule artifact,exactPath artifact)] of
        [proof] -> canonicalProofInterfaceBody proof artifact
        _ -> fail "retained interface lacks its prepared admission owner"

newPreparedWorldInterfaceArtifacts :: WorldStageInputs -> FilePath -> IO OriginalInterfaceArtifacts
newPreparedWorldInterfaceArtifacts inputs directory = withPreparedWorldStageInputs inputs $ \prepared world _ -> do
  let result = pprPipelineResult prepared
      captures = worldCellTypeInterfaces world
      selected = Map.fromList [((exactUnit artifact,exactModule artifact),(artifact,body)) | (artifact,body) <- captures]
      readInput artifact = case Map.lookup (exactUnit artifact,exactModule artifact) selected of
        Just (issued,body) | sameExactInterfaceIdentity issued artifact -> pure body
        _ -> case lookupStageType inputs (mkModule (stringToUnit (exactUnit artifact)) (mkModuleName (exactModule artifact))) of
          Just (issued,body) | sameExactInterfaceIdentity issued artifact -> pure body
          _ -> fail "World TYPE input has no bound capture"
  newOriginalInterfaceArtifactsWithLookup readInput (lookupStageType inputs) (prHscEnv result)
    (pprFinalizedModules prepared) (map fst captures)
    (prInjectedSessionInterfaces result) (prProducedSessionInterfaces result) directory

writeCertifiedProductsKeeping
  :: [FilePath] -> OriginalInterfaceArtifacts -> FilePath -> PreparedPipelineResult -> Maybe PreparedModuleProducts
  -> [(String, WireProgram)] -> IO CertifiedOriginalProducts
writeCertifiedProductsKeeping includes originalInterfaces outDir prepared productContext targets =
  writeCertifiedProductsKeepingWithOriginals includes originalInterfaces outDir prepared
    (fmap (\products -> PreparedProductContext products Map.empty (pprModules prepared) Nothing Set.empty Nothing Nothing emptyPreparedPackageOutputs) productContext) targets

writeCertifiedProductsKeepingWithOriginals
  :: [FilePath] -> OriginalInterfaceArtifacts -> FilePath -> PreparedPipelineResult -> Maybe PreparedProductContext
  -> [(String, WireProgram)] -> IO CertifiedOriginalProducts
writeCertifiedProductsKeepingWithOriginals includes originals directory prepared context targets =
  first <$> writeCertifiedProducts OrdinaryProductFacts False includes originals directory prepared context targets
  where first (certified,_,_) = certified

writeCertifiedSegmentProducts
  :: [FilePath] -> OriginalInterfaceArtifacts -> FilePath -> PreparedPipelineResult
  -> PreparedProductContext -> IO StagedOriginalProducts
writeCertifiedSegmentProducts includes originals directory prepared context = do
  (certified,bytes,outputs) <- writeCertifiedProducts SegmentOriginalFacts True includes originals directory prepared (Just context) []
  let current = finalizedTypeInterfaces (certifiedFinalizedArtifacts certified)
      prior = case preparedStageInputs context of Just (WorldStageInputs _ previous _ _ _ _ _) -> previous; Nothing -> Nothing
      candidates = Map.union current (originalRetainedInterfaceCaptures originals)
  additions <- foldM (\known (key,pair) -> case lookupCompletedType prior (pairOwner pair) of
      Nothing -> pure (Map.insert key pair known)
      Just old -> either fail (const (pure known))
        (retainTypeInterfaceCaptures (Map.singleton key old) (Map.singleton key pair))) Map.empty (Map.toAscList candidates)
  let canonical = canonicalOriginalInputAdditions (lookupCompletedCanonical prior) (pprCanonicalOriginalInputs prepared)
  pure (StagedOriginalProducts certified (directory </> "certified-products.cbor") bytes outputs
    (StageInputAdditions additions canonical))

writeCertifiedSegmentItemProducts
  :: PreparedPipelineResult -> StagedOriginalProducts -> FilePath -> PreparedPackageTarget -> IO FilePath
writeCertifiedSegmentItemProducts prepared (StagedOriginalProducts originals _ _ _ _) directory target = do
  context <- maybe (fail "item output lacks its segment original certification") pure
    (certifiedTargetContext originals)
  selection <- either fail pure (selectItemPackage context target)
  certified <- encodeCertifiedItemProductsWithInputs
    (prHscEnv (pprPipelineResult prepared)) context selection
  (certifiedBytes,_) <- either fail pure certified
  let name = "certified-products.cbor"
  BS.writeFile (directory </> name) certifiedBytes
  pure name

writeCertifiedSegmentItemProductsWithInputs
  :: PreparedPipelineResult -> StagedOriginalProducts -> FilePath -> PreparedPackageTarget
  -> IO (FilePath,Set.Set (SymbolIdentity,Word64))
writeCertifiedSegmentItemProductsWithInputs prepared (StagedOriginalProducts originals _ _ _ _) directory target = do
  context <- maybe (fail "item output lacks its segment original certification") pure
    (certifiedTargetContext originals)
  selection <- either fail pure (selectItemPackage context target)
  (bytes,inputs) <- encodeCertifiedItemProductsWithInputs
    (prHscEnv (pprPipelineResult prepared)) context selection >>= either fail pure
  let name = "certified-products.cbor"
  BS.writeFile (directory </> name) bytes
  pure (name,inputs)

-- A published original retains the values needed by its public runtime
-- exports and every instance dictionary in its actual interface inventory.
-- This reads the completed compiler owner; it neither recertifies declarations
-- nor treats the checking wrapper's unit root as the publication root.
stagedDeclarationInputReferences
  :: PreparedPipelineResult -> StagedOriginalProducts -> PlannedDeclarationInventory
  -> IO (Either String (Set.Set (SymbolIdentity,Word64)))
stagedDeclarationInputReferences prepared (StagedOriginalProducts originals _ _ _ _) inventory =
  case lookupHpt (hsc_HPT environment) (mkModuleName (snd owner)) of
    Nothing -> pure (Left "published declaration lost its completed compiler interface")
    Just home
      | let iface = hm_iface home
      , (unitString (moduleUnit (mi_module iface)),moduleNameString (moduleName (mi_module iface))) == owner
      , show (mi_iface_hash (mi_final_exts iface)) == plannedInterfaceFingerprint inventory -> do
          let iface = hm_iface home
              names = Set.toAscList (Set.fromList
                (concatMap availNames (mi_exports iface) ++ map ifDFun (mi_insts iface)))
          runtime <- mapM runtimeName names
          pure $ do
            roots <- Set.fromList . map preparedRootIdentity . concat <$> sequence runtime
            context <- maybe (Left "published declaration lacks its native certification context") Right
              (certifiedTargetContext originals)
            certifiedBinderInputClosure context roots
      | otherwise -> pure (Left "published declaration differs from its completed compiler interface")
  where
    environment = prHscEnv (pprPipelineResult prepared)
    owner = plannedOriginalOwner inventory
    runtimeName name = do
      thing <- lookupType environment name
      maybe (pure (Left "published declaration runtime export has no compiler identity")) runtimeIds thing
    runtimeIds (AnId identifier)
      -- Constructor applications have structural representation; constructor
      -- workers cannot capture values and have no recoverable interface body.
      | isJust (isDataConWorkId_maybe identifier) = pure (Right [])
      -- GHC's newtype workers and wrappers are DataConWrapIds with compulsory
      -- cast unfoldings and no top-level binding (Id.Make, Newtype workers).
      | Just constructor <- isDataConWrapId_maybe identifier
      , isNewTyCon (dataConTyCon constructor) = pure (Right [])
      | otherwise = pure (Right [identifier])
    runtimeIds thing@(ATyCon tycon) = do
      implicit <- mapM runtimeIds (implicitTyThings thing)
      -- Default implementations are named by GHC's class inventory, but are
      -- absent from implicitTyThings and may be used by a future instance.
      defaults <- mapM runtimeName
        [name | clas <- maybe [] pure (tyConClass_maybe tycon)
          , (_,Just (name,_)) <- classOpItems clas]
      pure (concat <$> sequence (implicit ++ defaults))
    runtimeIds (AConLike (PatSynCon patternSynonym)) = do
      let (matcher,_,_) = patSynMatcher patternSynonym
          builders = case patSynBuilder patternSynonym of
            Nothing -> []
            Just (builder,_,_) -> [builder]
      implementations <- mapM runtimeName (matcher:builders)
      pure (concat <$> sequence implementations)
    runtimeIds thing@(AConLike (RealDataCon _)) =
      fmap (fmap concat . sequence) (mapM runtimeIds (implicitTyThings thing))
    runtimeIds (ACoAxiom _) = pure (Right [])

publishStagedOriginalProducts :: PreparedPipelineResult -> StagedOriginalProducts -> IO CertifiedOriginalProducts
publishStagedOriginalProducts prepared (StagedOriginalProducts certified path bytes outputs _) = do
  revalidatePreparedCandidateInputs prepared
  let env = prHscEnv (pprPipelineResult prepared)
  case preparedExactCompilation prepared of
    Nothing -> do
      revalidateExactScopesAtWithOutputs RetainedProductsPublication env [] outputs >>= either fail pure
      BS.writeFile path bytes
    Just compilation -> writeCheckedExactCompilationWithOutputsAndPublication env compilation
      (certifiedFreshDependencies certified) outputs (PublishCertificate path bytes)
  pure certified

writeCertifiedProducts
  :: CertifiedProductKind -> Bool
  -> [FilePath] -> OriginalInterfaceArtifacts -> FilePath -> PreparedPipelineResult -> Maybe PreparedProductContext
  -> [(String, WireProgram)] -> IO (CertifiedOriginalProducts, BS.ByteString, FreshOutputSeals)
writeCertifiedProducts kind stageCertificate includes originalInterfaces outDir prepared productContext targets = do
    let hscEnv = prHscEnv (pprPipelineResult prepared)
    timing <- readTimingEnabled
    issued <- traverse (admitCurrentOriginalProducts originalInterfaces outDir prepared) productContext
    let PreparedPackageOutputs packageWrites packageInterfaces packageLinks =
          maybe emptyPreparedPackageOutputs preparedPackageOutputs issued
        capturedPackages = Map.map (\(path,body) -> (outDir </> path,body)) packageInterfaces
    forM_ packageWrites $ \(relative,bytes) -> do
      let path = outDir </> relative
      createDirectoryIfMissing True (takeDirectory path)
      BS.writeFile path bytes
    let dependencies = preparedFreshDependencies prepared
    capturedFinalization <- case issued >>= preparedCurrentOriginalInventory of
      Just inventory -> pure (currentOriginalFinalized inventory)
      Nothing -> timeDetailPhase timing "module_products" "capture_finalization" $
        capturePreparedFinalization originalInterfaces outDir prepared
          (freshSourceOwners dependencies)
    (finalized,finalizedOutputs) <- materializeFinalizedModuleArtifactsWithOutputs outDir capturedFinalization
    let retained = maybe Map.empty preparedRetainedOriginals issued
        retainedProofs = Map.fromList
          [((unitString (moduleUnit owner),moduleNameString (moduleName owner)),admittedOriginalProof original)
          | (owner,original) <- Map.toAscList retained]
        -- A demanded prior source has its actual finalized interface here;
        -- its source-less Finder location cannot reissue that compiler fact.
        selectedInterfaces = Map.union (Map.fromList
          [(moduleName owner,hm_iface (finalizedHomeModInfo (admittedOriginalModule original)))
          | (owner,original) <- Map.toAscList retained]) (pprProductInterfaces prepared)
        inventory = issued >>= preparedCurrentOriginalInventory
        availability = maybe Map.empty currentOriginalAvailability inventory
        freshEncodings = maybe [] currentOriginalProducts inventory
        freshProducts = map originalProductEncoding freshEncodings
        productPackages = maybe Map.empty currentOriginalPackages inventory
    (productBytes,inventoryOutputs) <- timeDetailPhase timing "module_products" "write_products" $
      writeProductInventory outDir freshEncodings
        [(unit,name,bytes) | ((unit,name),bytes) <- Map.toAscList productPackages]
    let withCertified = foldr (\candidate -> Map.insert
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
        finalDependencies = case preparedSourceRoute prepared of
          PreparedOrdinarySource -> freshDependencies
          _ -> freshDependencies
            { dependencyCacheSafe = False, dependencySelectionComplete = False }
    dependencyBytes <- timeDetailPhase timing "module_products" "dependency_evidence" $
      writeDependencyEvidence outDir finalDependencies
    let evidenceBytes = dependencyBytes
    sourceRecipe <- case preparedSourceRoute prepared of
      PreparedOrdinarySource -> pure OrdinaryExecutionSource
      PreparedWorldSource base domain -> pure
        (WorldExecutionSourceUnavailable base domain UnsupportedSourceRecipe)
      PreparedExactSource compilation -> issueFreshExecutionSource includes prepared freshDependencies
        [product | product <- freshProducts, let (unit,name,_,_) = moduleProductInput product
          , Map.notMember (T.unpack unit,T.unpack name) retainedProofs]
        productPackages (compilationScope compilation)
    executionOutput <- case sourceRecipe of
      ExactExecutionSourceAvailable graph -> do
        let bytes = executionGraphBytes graph
        BS.writeFile (outDir </> "execution-source.cbor") bytes
        pure [(outDir </> "execution-source.cbor",bytes)]
      _ -> pure []
    (retainedVersions,targetContext,certificateBytes,packageInterfaceWrites) <- timeDetailPhase timing "module_products" "certify" $ do
      let emittedSeals = Map.fromList
            [((unit,name),(version,T.pack (shaHex iface),T.pack (shaHex native),T.pack (shaHex packages)))
            | product <- freshProducts
            , let (unit,name,iface,_) = moduleProductInput product
            , let native = moduleProductBytes product
            , Just packages <- [Map.lookup (T.unpack unit,T.unpack name) productPackages]
            , let version = case (Map.lookup (T.unpack unit,T.unpack name) retainedProofs,
                    compilationScope <$> preparedExactCompilation prepared) of
                    (Nothing,Just scope) -> maybe "" (T.pack . (\source ->
                      exactProgramProductVersionFromDigest scope (T.unpack unit) (T.unpack name)
                        (T.unpack source) iface native packages)) (readyNativeProductSourceSha256 finalDependencies (T.unpack unit) (T.unpack name))
                    _ -> ""]
      reconciled <- case inventory of
        Just current -> pure (currentReconciledOriginalProducts current)
        Nothing -> either (ioError . userError) pure (reconcileOriginalProducts
          (compilationScope <$> preparedExactCompilation prepared) [])
      let previousContext = preparedPriorOriginalProducts =<< productContext
          priorContext = case previousContext of
            Nothing -> Nothing
            Just (StagedOriginalProducts previous _ _ _ _) -> certifiedTargetContext previous
      certified <- encodeCertifiedOriginalProductsWithPrior outDir priorContext (Just originalInterfaces)
        [(path,SHA256.hash bytes) | (path,bytes) <- packageWrites, takeDirectory path == "package-products"]
        capturedPackages packageLinks (pprOriginalReferences prepared) kind retainedProofs emittedSeals reconciled hscEnv sourceRecipe selectedInterfaces finalized (pprAcceptedCandidates prepared)
        (compilationScope <$> preparedExactCompilation prepared)
        (map moduleProductInput freshProducts) targets
        finalDependencies productBytes evidenceBytes
      case certified of
        Right (bytes,versions,context,writes) -> do
          pure (versions,case kind of OrdinaryProductFacts -> Nothing; SegmentOriginalFacts -> Just context,bytes,writes)
        Left reason -> do
          hPutStrLn stderr ("product certification unavailable: " ++ reason)
          fail ("native product certification failed: " ++ reason)
    let previousNames = case preparedPriorOriginalProducts =<< productContext of
          Just (StagedOriginalProducts previous _ _ _ _) ->
            let CertifiedNameBindings _ bindings = certifiedNameBindings previous in bindings
          Nothing -> Map.empty
        currentNames = maybe Map.empty currentOriginalNames inventory
        externalNames = maybe Map.empty currentOriginalExternalNames inventory
    knownNames <- mergeOriginalBindings previousNames externalNames >>= (`mergeOriginalBindings` currentNames)
    let nativeNames = Map.filter (\identity -> identity `Set.member` maybe Set.empty certifiedContextBinders targetContext) knownNames
        nameBindings = CertifiedNameBindings (nameCacheIdentity hscEnv) nativeNames
    -- Retain only Name and small identity facts, never lazy projections from
    -- the preparation's Id/details owner. The GHC details knots stay untouched.
    _ <- evaluate (Map.foldl' (\() identity ->
      symbolUnit identity `seq` symbolModule identity `seq` symbolNamespace identity `seq`
        symbolOccurrence identity `seq` maybe () (\parent -> parent `seq` ()) (symbolRecordParent identity))
      () nativeNames)
    sourceOriginals <- case preparedExactCompilation prepared of
      Nothing -> do
        let owner = tcg_mod (prTargetTcGblEnv (pprPipelineResult prepared))
        captureWorldSourceOriginals originalInterfaces prepared
          (unitString (moduleUnit owner),moduleNameString (moduleName owner)) finalized finalDependencies
      Just compilation -> do
        let owner = tcg_mod (prTargetTcGblEnv (pprPipelineResult prepared))
        captureFinalizedSourceOriginals compilation (pprAcceptedCandidates prepared)
          (unitString (moduleUnit owner),moduleNameString (moduleName owner)) finalized finalDependencies
    let previousCaptures = case preparedPriorOriginalProducts =<< productContext of
          Just (StagedOriginalProducts previous _ _ _ _) -> certifiedSourceCaptures previous
          Nothing -> Map.empty
    sourceCaptures <- foldM (\known (key,proof) -> do
      capture <- maybe (fail "source original certificate lacks its captured compiler owner") pure
        (Map.lookup key (finalizedLocalAdmissions finalized))
      case Map.lookup key known of
        Nothing -> pure (Map.insert key (SourceOriginalCapture proof capture) known)
        Just (SourceOriginalCapture old _) -> do
          unless (canonicalCertificateBytes old == canonicalCertificateBytes proof)
            (fail "source original conflicts with its earlier captured identity")
          pure known) previousCaptures (Map.toAscList sourceOriginals)
    outputSeals <- either fail pure $ freshOutputSealsFromWrites
      (finalizedOutputs
        ++ inventoryOutputs
        ++ packageInterfaceWrites
        ++ [(outDir </> path,bytes) | (path,bytes) <- packageWrites]
        ++ [(outDir </> "dependencies.json",dependencyBytes)]
        ++ executionOutput
        ++ [(canonicalCertificatePath proof,canonicalCertificateBytes proof)
            | proof <- Map.elems sourceOriginals])
    unless stageCertificate $ do
      revalidatePreparedCandidateInputs prepared
      let publishCertificate = BS.writeFile (outDir </> "certified-products.cbor") certificateBytes
      case preparedExactCompilation prepared of
        Just compilation -> writeCheckedExactCompilationWithOutputsAndPublication hscEnv compilation
          freshDependencies outputSeals (PublishCertificate (outDir </> "certified-products.cbor") certificateBytes)
        Nothing -> do
          revalidateExactScopesAtWithOutputs RetainedProductsPublication hscEnv [] outputSeals >>= either fail pure
          publishCertificate
    pure (CertifiedOriginalProducts freshProducts sourceCaptures finalized sourceRecipe retainedProofs retainedVersions targetContext nameBindings freshDependencies, certificateBytes, outputSeals)


-- An immutable original product needs captured finalized Core as well as its
-- interface. Unsupported Core remains interface-only; native target projection
-- and required global ownership still enforce their own complete contracts.
admitModuleProducts :: OriginalInterfaceArtifacts -> PreparedProductContext
  -> Map.Map (String,String) LocalFinalizedAdmission
  -> Map.Map ModuleName ModIface
  -> Map.Map ModuleName PackageImportEvidence
  -> IO (Map.Map (String, String) ProductAvailability,
         [ObservedOriginalProduct], Map.Map (String,String) BS.ByteString)
admitModuleProducts originalInterfaces productContext finalized interfaces packageRoots = do
  let inventory = preparedProductInventory productContext
  timing <- readTimingEnabled
  forM_ (preparedModuleProductOmissions inventory) $ \(owner, omissions) ->
    forM_ omissions $ \omission ->
      hPutStrLn stderr ("module product group omitted: " ++ unitString (moduleUnit owner) ++ ":" ++ moduleNameString (moduleName owner)
        ++ "#" ++ show (omittedOriginalOrdinal omission) ++ " "
        ++ show (omittedOriginalBinders omission) ++ " "
        ++ show (omittedOriginalReason omission))
  outcomes <- forM (preparedModuleProductEncodings inventory) $ \(owner, outcome) -> do
    let name = moduleName owner
        key = (unitString (moduleUnit owner), moduleNameString name)
    let selectedInterface = Map.lookup owner retained >>= \original ->
          Just (hm_iface (finalizedHomeModInfo (admittedOriginalModule original)))
    case (selectedInterface <|> Map.lookup name interfaces) >>= \interface ->
        if mi_module interface == owner then Just interface else Nothing of
      Nothing -> do
        hPutStrLn stderr ("module product unavailable: no interface for " ++ moduleNameString name)
        pure (key, ProductMissingInterface, Nothing, Nothing)
      Just _ -> do
        canIssue <- if Map.member owner retained then pure True else case Map.lookup key finalized of
          Nothing -> throwIO (CandidateOriginalAuthorityMissing (fst key) (snd key))
          Just original -> pure (isJust (localFinalizedCore original))
        if not canIssue then do
          hPutStrLn stderr ("module product unavailable: " ++ moduleNameString name
            ++ ": finalized Core cannot issue an immutable native original")
          pure (key, ProductInterfaceOnly, Nothing, Nothing)
        else case outcome of
          Left reason -> do
            hPutStrLn stderr ("module product unavailable: " ++ moduleNameString name
              ++ ": " ++ show reason)
            pure (key, ProductProjectionRejected, Nothing, Nothing)
          -- A captured interface with no current groups carries metadata only.
          -- It cannot issue a native owner without a current executable group;
          -- prior native ownership remains in the staged certification context.
          Right [] -> pure (key, ProductInterfaceOnly, Nothing, Nothing)
          Right groups -> issue timing key name owner groups
  let products = [moduleProduct | (_, _, Just moduleProduct, _) <- outcomes]
      packageBundles =
        [(unit, moduleName', sidecar)
        | ((unit, moduleName'), _, Just _, Just sidecar) <- outcomes]
  pure (Map.fromList [(key, status) | (key, status, _, _) <- outcomes], products,
    Map.fromList [((unit,name),bytes) | (unit,name,bytes) <- packageBundles])
  where
    retained = preparedRetainedOriginals productContext
    issue timing key name owner groups = do
      bytes <- timeDetailPhase timing "module_products.interfaces" (snd key) $
        originalInterfaceBytes originalInterfaces owner
          >>= maybe (fail "original product interface lacks its captured artifact") pure
      sidecar <- case Map.lookup owner retained of
        Just original -> do
          let (iface,path,seal) = admittedOriginalInterface original
          captured <- canonicalProofOriginalBytes (admittedOriginalProof original) path seal
          unless (shaHex captured == seal && shaHex bytes == exactSha256 iface)
            (fail "retained original interface or package capture changed")
          pure captured
        Nothing -> do
          roots <- case Map.lookup name packageRoots of
            Nothing -> ioError (userError
              ("resolved direct package import inventory missing for " ++ moduleNameString name))
            Just selected -> pure selected
          original <- maybe (fail "fresh original product lacks its captured finalization") pure
            (Map.lookup key finalized)
          let (iface,_,_) = localFinalizedInterface original
          unless ((exactUnit iface,exactModule iface) == key
              && exactSha256 iface == shaHex bytes
              && localFinalizedPackageEvidence original == roots)
            (fail "fresh original package evidence differs from its captured finalization")
          pure (artifactBytes (localFinalizedPackageBody original))
      when (BS.length sidecar > 4 * 1024 * 1024) $
        ioError (userError "direct package import witness exceeds four MiB")
      let encoded = prepareModuleProductEncodingFromGroups
            (T.pack (fst key)) (T.pack (snd key)) bytes groups
          observation = do
            raw <- preparedRawProducts productContext
            case filter ((== owner) . rawOriginalProductOwner) raw of
              [selected] -> rawProjectionObservation selected
              _ -> Nothing
      pure (key, ProductReady, Just (ObservedOriginalProduct encoded observation), Just sidecar)

-- Interface-only captures emit the same valid inventory framing with no native
-- rows. Product absence never prevents retaining the actual finalization.
writeProductInventory :: FilePath -> [ObservedOriginalProduct] -> [(String,String,BS.ByteString)] -> IO (BS.ByteString,[(FilePath,BS.ByteString)])
writeProductInventory outDir products packageBundles = do
  timing <- readTimingEnabled
  let productBytes = encodeModuleProductInventory (map originalProductEncoding products)
      packageBytes = toStrictByteString (encodeListLen 3
        <> encodeString (T.pack "TPPKGBUNDLES") <> encodeWord 1
        <> encodeListLen (fromIntegral (length packageBundles))
        <> foldMap (\(unit, moduleName', sidecar) -> encodeListLen 3
          <> encodeString (T.pack unit) <> encodeString (T.pack moduleName')
          <> encodeBytes sidecar) packageBundles)
  timeDetailPhase timing "module_products" "encode_products" $
    BS.writeFile (outDir </> "module-products.cbor") productBytes
  when timing $ forM_ products $ \(ObservedOriginalProduct encoding observation) -> do
    let (unit,owner,_,groups) = moduleProductInput encoding
    emitOriginalGroupWork timing Nothing AcceptedOriginalOutput (T.unpack unit) (T.unpack owner)
      observation (map projectedOriginalOrdinal groups) []
      (Just (outDir </> "module-products.cbor")) Nothing
  emitCount timing "original_inventory_written_bytes" (fromIntegral (BS.length productBytes))
  timeDetailPhase timing "module_products" "encode_package_bundles" $
    BS.writeFile (outDir </> "module-package-imports.cbor") packageBytes
  pure (productBytes,
    [(outDir </> "module-products.cbor",productBytes)
    ,(outDir </> "module-package-imports.cbor",packageBytes)])


-- A later item can execute a quoter defined by an original retained here.
-- Keep its consumed source recipe at the same boundary as its native product,
-- rather than waiting for the frontend's postworker item certification.
issueFreshExecutionSource
  :: [FilePath] -> PreparedPipelineResult -> DependencyEvidence -> [ModuleProductEncoding]
  -> Map.Map (String,String) BS.ByteString -> ExactScope -> IO WorkerExecutionSource
issueFreshExecutionSource includes prepared evidence fullProducts productPackages scope
  | null fullProducts = pure (ExactExecutionSourceUnavailable NoFreshOriginals)
  | not (dependencyCacheSafe evidence && dependencySelectionComplete evidence) =
      pure (ExactExecutionSourceUnavailable IncompleteSourceEvidence)
  | otherwise = do
      compilation <- maybe (fail "fresh supporting original has no compiler transaction") pure
        (preparedExactCompilation prepared)
      let exactRows = compilationExactImports compilation
      if any (\((_,_,boot),edges) -> boot || any (\(_,_,isBoot,_) -> isBoot) edges) exactRows
        then pure (ExactExecutionSourceUnavailable UnsupportedSourceRecipe)
        else do
          origin <- normalise <$> makeAbsolute (compilationSource compilation)
          sourceBytes <- BS.readFile origin
          case [source | source <- dependencySources evidence, dependencySourcePath source == origin] of
            [source] | dependencySourceSha256 source == shaHex sourceBytes -> pure ()
            _ -> throwIO (ExecutionSourceChanged ("","compiler source recipe"))
          validateDependencyEvidence evidence
          source <- either (const (throwIO (ExecutionSourceUnsupported ("","compiler source recipe"))))
            (pure . T.unpack) (TE.decodeUtf8' sourceBytes)
          allRootsPresent <- and <$> mapM doesPathExist includes
          roots <- if allRootsPresent then mapM canonicalizePath includes else pure []
          fresh <- mapM freshIdentity fullProducts
          sourcePackages <- forM fullProducts $ \product' -> do
            let (_,owner,_,_) = moduleProductInput product'
            maybe (fail "fresh execution original lacks package witness") pure
              (Map.lookup (mkModuleName (T.unpack owner)) (pprPackageImports prepared))
          packages <- foldM retainPackage Map.empty (concatMap packageInterfaces sourcePackages)
          let freshKeys = Set.fromList (map executionIdentityKey fresh)
              originalIdentity product' = ExecutionSourceIdentity
                (originalUnit product') (originalModule product') (originalVersion product')
                (originalIfaceSha256 product') (originalProductSha256 product')
              retained = map originalIdentity (scopeRecipeOriginals scope)
                ++ [ExecutionSourceIdentity (candidateUnit candidate) (candidateModule candidate)
                    (candidateModuleVersion candidate) (candidateInterfaceSha256 candidate)
                    (candidateProductSha256 candidate) | candidate <- pprAcceptedCandidates prepared]
          inherited <- either throwIO pure (executionSourceInheritedOwners
            (scopeExecutionOwners scope ++ map snd (mapMaybe candidateExecutionSources
              (pprAcceptedCandidates prepared)))
            [original | original <- retained, executionIdentityKey original `Set.notMember` freshKeys])
          let normalized = evidence
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
          pure $ case issued of
            Just graph -> ExactExecutionSourceAvailable graph
            Nothing -> ExactExecutionSourceUnavailable
              (if allRootsPresent then UnsupportedSourceRecipe else UnavailableSourceRoot)
  where
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
      packageBytes <- maybe (fail "fresh execution original lacks issued package bytes") pure
        (Map.lookup (unit,owner) productPackages)
      let nativeBytes = moduleProductBytes product'
          expected = ExecutionSourceIdentity unit owner
            (exactProgramProductVersionFromDigest scope unit owner sourceDigest interfaceBytes nativeBytes packageBytes)
            (shaHex interfaceBytes) (shaHex nativeBytes)
      case [ExecutionSourceIdentity unit owner (originalVersion original)
              (originalIfaceSha256 original) (originalProductSha256 original)
            | original <- scopeProducts scope
            , (originalUnit original,originalModule original) == (unit,owner)] of
        [original] | original == expected -> pure original
        [] -> pure expected
        _ -> throwIO (ExecutionSourceConflicting (unit,owner))
    retainPackage selected root =
      let key = (packageUnit root,packageModule root)
      in case Map.lookup key selected of
        Nothing -> pure (Map.insert key root selected)
        Just previous | previous == root -> pure selected
        _ -> throwIO (ExecutionSourceConflicting key)


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

retainProgramProducts
  :: FilePath -> PreparedPipelineResult
  -> CertifiedOriginalProducts -> String -> ExactScope -> IO ExactScope
retainProgramProducts = retainProgramProductsWithPublication Nothing

retainStagedProgramProducts
  :: FilePath -> PreparedPipelineResult
  -> StagedOriginalProducts -> String -> ExactScope -> IO ExactScope
retainStagedProgramProducts directory prepared (StagedOriginalProducts certified path bytes seals _) target initial =
  retainProgramProductsWithPublication
    (Just (path,bytes,seals)) directory prepared certified target initial

retainProgramProductsWithPublication
  :: Maybe (FilePath,BS.ByteString,FreshOutputSeals) -> FilePath -> PreparedPipelineResult
  -> CertifiedOriginalProducts -> String -> ExactScope -> IO ExactScope
retainProgramProductsWithPublication stagedCertificate directory prepared certified target initial = do
  outputWrites <- newIORef []
  selected <- either fail pure (extendSourceSelectedOriginals
    (preparedExactCompilation prepared >>= compilationSourceSelection) initial)
  finalized <- foldM retainInterface (selected,[],[],[]) (Map.toAscList localInterfaces)
  (cached,additions,pendingProducts,pendingLexical) <- foldM (retainCached outputWrites) finalized (zip [0::Int ..] (pprCandidateAdmissions prepared))
  admitted <- extendExactScopeGeneration cached additions (map CompilerOriginal pendingProducts) pendingLexical >>= either fail pure
  promoted <- foldM (retain outputWrites) admitted (zip [0::Int ..] products)
  let parcels = mapMaybe candidateExecutionSources (pprAcceptedCandidates prepared)
  inherited <- either throwIO pure
    (extendExactExecutionSources (concatMap fst parcels) (map snd parcels) promoted)
  retained <- case certifiedExecutionSource certified of
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
    WorldExecutionSourceUnavailable _ _ _ -> fail "World products cannot extend a finite exact source scope"
    OrdinaryExecutionSource -> fail "exact program products lack exact source recipe outcome"
  revalidatePreparedCandidateInputs prepared
  retainedSeals <- either fail pure . freshOutputSealsFromWrites =<< readIORef outputWrites
  outputSeals <- case stagedCertificate of
    Nothing -> pure retainedSeals
    Just (_,_,initialSeals) -> either fail pure (appendFreshOutputSeals initialSeals retainedSeals)
  let env = prHscEnv (pprPipelineResult prepared)
      publishStaged = forM_ stagedCertificate $ \(path,bytes,_) -> BS.writeFile path bytes
  case preparedExactCompilation prepared of
    Nothing -> do
      revalidateExactScopesAtWithOutputs RetainedProductsPublication env [retained] outputSeals >>= either fail pure
      publishStaged
    Just compilation -> case stagedCertificate of
      Nothing -> writeRetainedExactCompilationWithOutputsAndPublication env retained compilation
        (certifiedFreshDependencies certified) outputSeals ReceiptOnly
      Just (path,bytes,_) -> writeRetainedExactCompilationWithOutputsAndPublication env retained compilation
        (certifiedFreshDependencies certified) outputSeals (PublishCertificate path bytes)
  pure retained
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
    retainCached outputWrites (scope,additions,stagedProducts,stagedLexical) (index, admission) = do
      let candidate = preparedCandidateOriginal admission
          proof = preparedCandidateProof admission
          unit = candidateUnit candidate
          owner = candidateModule candidate
          key = (unit,owner)
      interfaceBytes <- canonicalProofOriginalBytes proof (candidateInterface candidate) (candidateInterfaceSha256 candidate)
      packageBytes <- canonicalProofOriginalBytes proof (candidatePackageImports candidate) (candidatePackageImportsSha256 candidate)
      productBytes <- canonicalProofOriginalBytes proof (candidateProductPath candidate) (candidateProductSha256 candidate)
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
          writeFresh outputWrites interfacePath interfaceBytes
          writeFresh outputWrites packagesPath packageBytes
          writeFresh outputWrites productPath productBytes
          relocated <- either fail pure (relocateCanonicalInterfaceProof proof
            (interface,packagesPath,candidatePackageImportsSha256 candidate)
            (Just (productPath,candidateProductSha256 candidate)))
          pure (scope,additions ++ [((interface,packagesPath,candidatePackageImportsSha256 candidate),ModuleInterfaceEvidence relocated)],
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
              currentInterface <- scopeOriginalBytes scope (exactPath interface) (exactSha256 interface)
              currentPackages <- scopeOriginalBytes scope packagesPath packagesSha
              currentProduct <- scopeOriginalBytes scope (originalProductPath original) (originalProductSha256 original)
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
    retain outputWrites scope (index, originalProduct) = do
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
      packageBytes <- case Map.lookup key (certifiedRetainedOriginals certified) of
        Just original -> canonicalProofOriginalBytes original packagesPath packagesSha
        Nothing -> BS.readFile packagesPath
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
      writeFresh outputWrites productPath productBytes
      extendExactScopeGeneration scope [] [if Map.member key (certifiedRetainedOriginals certified)
        then PrivateNative original else CompilerOriginal original] [] >>= either fail pure
    writeFresh outputWrites path bytes = do
      BS.writeFile path bytes
      modifyIORef' outputWrites (++ [(path,bytes)])

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

programSourceRequirements :: PreparedPipelineResult -> String -> String -> IO [(String, String)]
programSourceRequirements prepared unit owner =
  either fail pure (preparedHomeRequirements prepared unit owner)
