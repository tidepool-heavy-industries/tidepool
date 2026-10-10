{-# LANGUAGE OverloadedStrings #-}

-- | Emit the compiler's complete original-product result once. Capture paths
-- belong to the supplied output directory; original source identities and
-- independently admitted candidate/exact evidence remain compiler inputs.
module Tidepool.CompilerProducts
  ( CertifiedOriginalProducts, certifiedOriginalProducts, certifiedFinalizedArtifacts
  , certifiedSourceOriginals, certifiedExecutionSource, writeCertifiedProductsKeeping, retainedOriginalInterfaces, newPreparedOriginalInterfaceArtifacts, retainProgramProducts, programLexicalRequirements, programSourceRequirements
  , certifiedRetainedOriginals, certifiedRetainedNativeVersions, PreparedProductContext, prepareOriginalProducts, prepareOriginalProductsWithExecutor
  , requireOriginalExecutableGlobals
  , writeCertifiedProductsKeepingWithOriginals
  , StagedOriginalProducts, stagedCertifiedOriginalProducts
  , writeCertifiedSegmentProducts, writeCertifiedSegmentItemProducts
  , publishStagedOriginalProducts, retainStagedProgramProducts
  , prepareCompilerProjectionContext, prepareCompilerProjectionContextForEnvironment, exactProgramProductVersionFromDigest
  , OriginalProjectionCollector, newOriginalProjectionCollector, observeOriginalProjection
  , prepareOriginalProductsWithCollector
  , prepareOriginalProductsWithCache
  , OriginalProductWorklist, observeOriginalProjectionWithRecovery, prepareOriginalProductsWithWorklist
  , CurrentOriginalInventory, admitCurrentOriginalProducts, preparedCurrentOriginalInventory
  , preparedProductInventory, currentOriginalBinders, currentOriginalBindingsExcept
  , currentReconciledOriginalProducts
  ) where

import Codec.CBOR.Encoding
import Codec.CBOR.Write (toStrictByteString)
import Control.Applicative ((<|>))
import Control.Exception (throwIO, evaluate)
import Control.Monad (foldM, forM, forM_, unless, when)
import Data.Bits (shiftR)
import Data.ByteString qualified as BS
import Data.IORef (newIORef, readIORef, writeIORef, modifyIORef', atomicModifyIORef')
import Data.Map.Strict qualified as Map
import Data.Maybe (mapMaybe, isJust, fromMaybe, isNothing)
import Data.Set qualified as Set
import Data.Text qualified as T
import Data.Text.Encoding qualified as TE
import Data.Word (Word64)
import GHC.Tc.Types (tcg_mod)
import GHC.Types.Name (Name)
import GHC.Types.Var (Id)
import GHC.Driver.Env (HscEnv, hsc_all_home_unit_ids)
import GHC.Unit.Module (Module, ModuleName, moduleName, moduleNameString, moduleUnit, mkModuleName, mkModule)
import GHC.Unit.Module.ModIface (ModIface, mi_module, mi_iface_hash, mi_final_exts)
import GHC.Unit.Types (unitString, stringToUnit, toUnitId)
import Numeric (readHex)
import System.Directory (canonicalizePath, doesPathExist, makeAbsolute)
import System.FilePath (normalise, (</>))
import System.IO (hPutStrLn, stderr)
import System.Info qualified as SystemInfo
import System.Mem.StableName (makeStableName)
import Tidepool.CertifiedProducts
  ( CertifiedProductKind(..), TargetCertificationContext, encodeCertifiedOriginalProducts
  , encodeCertifiedItemProducts, sourceProductSha256 )
import Tidepool.OriginalProductRoots
  ( ReconciledOriginalProducts, reconcileOriginalProducts )
import Tidepool.DependencyEvidence
import Tidepool.ExactHydration
  ( OriginalInterfaceArtifacts, ExactIfaceArtifact(..), originalInterfaceBytes, newOriginalInterfaceArtifactsWithReader )
import Tidepool.ExactScope
  ( ExactScope , scopeProducerSha256, scopeSemanticSha256, scopeProducts, scopeExecutionOwners, scopeInterfaces, ExactCompilation(..), ExactProduct(..), ExactOriginalGroup(..), scopeValueInterfaces
  , ExactScopeValidationReason(..), revalidateExactScopesAt, writeCheckedExactCompilation, writeCheckedExactCompilationWithPublication, writeRetainedExactCompilation, writeRetainedExactCompilationWithPublication, scopeCanonicalInterfaces, scopeInterfaceToken, scopeInterfaceToken, scopeOriginalBytes, canonicalProofInterfaceBody, canonicalProofOriginalBytes, relocateCanonicalInterfaceProof
  , CanonicalInterfaceProof, captureFinalizedSourceOriginals, originalGroupFromProjected, originalGroupFromCandidate
  , extendSourceSelectedOriginals, extendExactScopeGeneration, extendExactExecutionSources, extendExactExecutionSourcesWithinBudget
  , scopeLexical, scopeInterfaceEvidence, scopeExecutionGraphs, ExactInterfaceEvidence(..), canonicalCertificateSha256, canonicalSourceSha256 )
import Tidepool.ExecutionEncode
  ( ModuleProductEncoding, moduleProductInput, moduleProductBytes
  , prepareModuleProductEncoding, prepareModuleProductEncodingFromGroups, encodeModuleProductInventory )
import Tidepool.ExecutionProjection
  ( ProjectionContext(..), ProjectionError(..), PreparedModuleProducts, OriginalGroupOmission(..)
  , preparedModuleProductOutcomes, preparedModuleProductOmissions, resolveTextPackageUnit
  , rawOriginalProductOwner
  , OriginalProjectionCache, newOriginalProjectionCache, projectCachedOriginalHomeModuleProducts
  , lookupCachedOriginalHomeModuleProducts
  , rawOriginalProductBinders, rawOriginalProductDemands
  , rawOriginalGroupEncodings
  , settleOriginalHomeModuleProducts, settleOriginalHomeModuleProductsWithoutOwners
  , RawModuleProducts, preparedTopIdentityBindings )
import Tidepool.ExecutionSchema
  ( Architecture(..), Endianness(..), SymbolIdentity(..), TargetDescriptor(..), WireProgram
  , GlobalDecl(..), ProjectedGroup(..) )
import qualified Tidepool.ExecutionSchema as Execution
import Tidepool.ExecutionSource
  ( WorkerExecutionSource(..), SourceRecipeUnavailable(..), ExecutionSourceRecipe(..)
  , ExecutionSourceGraph(..), executionGraphBytes, executionGraphSha256, ExecutionSourceIdentity(..), ExecutionSourceOwner(..)
  , ExecutionSourceFailure(..), executionIdentityKey, issueExecutionSourceRecipe
  , executionSourceInheritedOwners, ExecutionSourceRef(..), executionSourceProspectiveReferences )
import Tidepool.ExtractUtil (shaHex)
import Tidepool.FinalizedModuleArtifacts
  ( FinalizedModuleArtifacts, captureFinalizedModuleArtifacts, materializeFinalizedModuleArtifacts, LocalFinalizedAdmission
  , finalizedLocalAdmissions, localFinalizedCore, localFinalizedInterface, localFinalizedSourceSha256 )
import Tidepool.GhcPipeline
  ( PreparedPipelineResult(..), pprAcceptedCandidates, PipelineResult(..), PreparedModuleObserver(..), PreparedModuleCompletionInputs(..)
  , preparedFreshDependencies, preparedExactCompilation, preparedHomeRequirements
  , preparedCandidateOriginal, preparedCandidateProof, revalidatePreparedCandidateInputs )
import Tidepool.ModuleCandidates (ModuleCandidate(..), candidateExecutionSources)
import Tidepool.PackageWitness (PackageImportEvidence(..), PackageImportRoot(..), encodePackageImports)
import Tidepool.PreparedFormatting (resolveFormattingAuthority)
import Tidepool.PreparedJson (JsonAuthority, resolveJsonAuthorityWithCanonicalInterfaces)
import Tidepool.PreparedTime (resolveTimeAuthority)
import Tidepool.PreparedStg
  ( PreparedModule, pmModule, pmSitedSiblings, runPreparedModuleTask
  , PreparedBodyCache, PreparedBodyReuse(..), newPreparedBodyCache, newPreparedOriginalModuleTaskPreparer )
import Tidepool.CompilerExecution (CompilerExecutor, withCompilerExecutor, serialCompilerExecutionGrant, runCompilerTasks)
import Tidepool.HomeProducts
  ( AdmittedFinalizedOriginal, admittedOriginalModule
  , admittedOriginalProof, admittedOriginalInterface, originalVersionLookup, originalVersionSeal )
import Tidepool.FinalizedModule (finalizedHomeModInfo)
import GHC.Unit.Home.ModInfo (hm_iface)
import Tidepool.Timing
  ( readTimingEnabled, timePhase, timeDetailPhase, timeModuleDetailPhase, emitCount
  , ReuseContext, ReuseModule(..), ReuseStage(..), ReuseDecision(..), ReuseReason(..)
  , ReuseVersionKind(..), emitReuse, emitReuseComplete )

-- The production worker and original-product fixtures share the compiler
-- profile and native package/type authorities. Callers select only their real
-- root/target and independently admitted host inputs.
prepareCompilerProjectionContext
  :: PreparedPipelineResult -> Map.Map SymbolIdentity Word64 -> Module -> String -> [String]
  -> Maybe JsonAuthority -> IO ProjectionContext
prepareCompilerProjectionContext prepared retainedGenerations owner target auxiliaryRoots hostJsonAuthority = do
  context <- prepareCompilerProjectionContextForEnvironment
    (prHscEnv (pprPipelineResult prepared))
    (compilationScope <$> preparedExactCompilation prepared)
    retainedGenerations owner target auxiliaryRoots hostJsonAuthority
  pure context {projectionCurrentOriginals = pprOriginalBindings prepared}

prepareCompilerProjectionContextForEnvironment
  :: HscEnv -> Maybe ExactScope -> Map.Map SymbolIdentity Word64 -> Module -> String -> [String]
  -> Maybe JsonAuthority -> IO ProjectionContext
prepareCompilerProjectionContextForEnvironment environment exactScope retainedGenerations owner target auxiliaryRoots hostJsonAuthority = do
  timing <- readTimingEnabled
  let symbol occurrence = SymbolIdentity (T.pack (unitString (moduleUnit owner)))
        (T.pack (moduleNameString (moduleName owner))) "value" (T.pack occurrence) Nothing
  formattingAuthority <- timePhase timing "formatting_authority" $ resolveFormattingAuthority environment
  timeAuthority <- timePhase timing "time_authority" $ resolveTimeAuthority environment
  jsonAuthority <- timePhase timing "json_authority" $ case hostJsonAuthority of
    Just authority -> pure (Just authority)
    Nothing -> resolveJsonAuthorityWithCanonicalInterfaces environment
      (maybe Map.empty scopeCanonicalInterfaces exactScope)
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
    environment interfaces owner exact inputs = do
  baseContext <- prepareCompilerProjectionContextForEnvironment environment exact retained owner
    "__original_projection" auxiliaryRoots json
  let context = baseContext {projectionCurrentOriginals = completionExternalOriginalBindings inputs}
      observe prepared = do
        _ <- projectCachedOriginalHomeModuleProducts completed environment interfaces context prepared
        pure ()
  pure (PreparedModuleObserver observe (\_ -> pure ()))

data CertifiedOriginalProducts = CertifiedOriginalProducts
  { certifiedOriginalProducts :: [ModuleProductEncoding]
  , certifiedSourceOriginals :: Map.Map (String,String) CanonicalInterfaceProof
  , certifiedFinalizedArtifacts :: FinalizedModuleArtifacts
  , certifiedExecutionSource :: WorkerExecutionSource
  , certifiedRetainedOriginals :: Map.Map (String,String) CanonicalInterfaceProof
  , certifiedRetainedNativeVersions :: Map.Map (String,String) String
  , certifiedTargetContext :: Maybe TargetCertificationContext
  }

-- Segment facts remain in memory until the segment's terminal proof succeeds.
-- Item products may borrow the target context without publishing this authority.
data StagedOriginalProducts = StagedOriginalProducts
  CertifiedOriginalProducts FilePath BS.ByteString

stagedCertifiedOriginalProducts :: StagedOriginalProducts -> CertifiedOriginalProducts
stagedCertifiedOriginalProducts (StagedOriginalProducts certified _ _) = certified

data PreparedProductContext = PreparedProductContext
  { preparedProductInventory :: PreparedModuleProducts
  , preparedRetainedOriginals :: Map.Map Module AdmittedFinalizedOriginal
  , preparedProductModules :: [PreparedModule]
  , preparedRawProducts :: Maybe [RawModuleProducts]
  , preparedExternalBinders :: Set.Set SymbolIdentity
  , preparedCurrentOriginalInventory :: Maybe CurrentOriginalInventory
  }

-- This owner issues the batch only after original Core, exact interface bytes,
-- and package witnesses have passed the publication checks. The writer consumes
-- these same immutable encodings rather than making a second availability choice.
data CurrentOriginalInventory = CurrentOriginalInventory
  { currentOriginalAvailability :: Map.Map (String,String) ProductAvailability
  , currentOriginalProducts :: [ModuleProductEncoding]
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
    , let (_,_,_,groups) = moduleProductInput product'
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
        , let (_,_,_,groups) = moduleProductInput product'
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
      finalized <- timeDetailPhase timing "module_products" "capture_finalization" $
        captureFinalizedModuleArtifacts originalInterfaces environment
          (pprFinalizedModules prepared) (pprPackageImports prepared)
          (preparedFreshDependencies prepared) outDir
      let withheld = Set.fromList
            [owner | prepared' <- preparedProductModules productContext
              , let owner = pmModule prepared'
                    key = (unitString (moduleUnit owner),moduleNameString (moduleName owner))
              , Map.notMember owner (preparedRetainedOriginals productContext)
              , maybe True (not . isJust . localFinalizedCore)
                  (Map.lookup key (finalizedLocalAdmissions finalized))]
          admittedContext = case preparedRawProducts productContext of
            Nothing -> productContext
            Just raw | not (Set.null withheld) -> productContext {preparedProductInventory = fst
              (settleOriginalHomeModuleProductsWithoutOwners environment
                (preparedExternalBinders productContext) withheld raw)}
            Just _ -> productContext
      (availability,products,packages) <- admitModuleProducts originalInterfaces admittedContext
        (finalizedLocalAdmissions finalized) (pprProductInterfaces prepared)
        (pprPackageImports prepared)
      reconciled <- either (ioError . userError) pure (reconcileOriginalProducts
        (compilationScope <$> preparedExactCompilation prepared)
        [(T.unpack unit, T.unpack name, map originalGroupFromProjected groups)
          | product <- products, let (unit,name,_,groups) = moduleProductInput product])
      let inventory = CurrentOriginalInventory
            { currentOriginalAvailability = availability
            , currentOriginalProducts = products
            , currentOriginalPackages = packages
            , currentOriginalFinalized = finalized
            , currentReconciledOriginalProducts = reconciled
            , currentOriginalNames = preparedTopIdentityBindings (preparedProductModules productContext)
            , currentOriginalExternalNames = pprOriginalBindings prepared
            }
      pure admittedContext {preparedCurrentOriginalInventory = Just inventory}


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
  withCompilerExecutor serialCompilerExecutionGrant $ \executor ->
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
prepareOriginalProductsWithCache cache collector = prepareOriginalProductsUsingCollector collector (Just cache)

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
  baseContext <- prepareCompilerProjectionContextForEnvironment env exact retained owner
    "__original_projection" auxiliaryRoots json
  let context = baseContext {projectionCurrentOriginals = completionExternalOriginalBindings inputs}
  worklist <- newOriginalProductWorklist collector cache executor (Just (completionReuseContext inputs)) env exact interfaces context
    (completionExternalOriginals inputs) (completionSourceOwners inputs) (completionSiblings inputs)
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
  prepareOriginalProductsUsingCollectorWithReuse collector stable executor Nothing

prepareOriginalProductsUsingCollectorWithReuse
  :: Maybe OriginalProjectionCollector -> Maybe PreparedBodyCache -> CompilerExecutor
  -> Maybe ReuseContext -> HscEnv -> Maybe ExactScope -> Map.Map ModuleName ModIface
  -> ProjectionContext -> Set.Set SymbolIdentity -> [PreparedModule]
  -> IO ([PreparedModule],PreparedProductContext)
prepareOriginalProductsUsingCollectorWithReuse collector stable executor reuse env exact interfaces context external initial = do
  cache <- maybe newPreparedBodyCache pure stable
  completedRaw <- maybe newOriginalProjectionCache pure collector
  worklist <- newOriginalProductWorklist completedRaw cache executor reuse env exact interfaces context external
    (Set.fromList (map pmModule initial)) (Map.unions (map pmSitedSiblings initial))
  _ <- runCompilerTasks executor
    (\prepared -> worklistObserve worklist prepared >> pure prepared)
    (\_ prepared -> worklistComplete worklist prepared) initial
  worklistFinish worklist

newOriginalProductWorklist :: OriginalProjectionCollector -> PreparedBodyCache -> CompilerExecutor
  -> Maybe ReuseContext -> HscEnv -> Maybe ExactScope -> Map.Map ModuleName ModIface -> ProjectionContext
  -> Set.Set SymbolIdentity -> Set.Set Module -> Map.Map String Id
  -> IO OriginalProductWorklist
newOriginalProductWorklist completedRaw cache executor reuse env exact interfaces context external sourceOwners frozenSiblings = do
  timing <- readTimingEnabled
  workRef <- newIORef (0 :: Integer,0 :: Integer)
  reuseRef <- newIORef (0 :: Integer,0 :: Integer)
  reportedRef <- newIORef False
  originalPreparerRef <- newIORef Nothing
  modulesRef <- newIORef Map.empty
  admittedRef <- newIORef Map.empty
  attemptedRef <- newIORef Set.empty
  rawRef <- newIORef Map.empty
  knownRef <- newIORef external
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
      acquireOriginal siblings owner = case exact of
        Nothing -> pure Nothing
        Just scope -> do
          when (any (\original -> (originalUnit original,originalModule original) ==
              (unitString (moduleUnit owner),moduleNameString (moduleName owner))) (scopeProducts scope))
            (fail ("required binder is absent from the admitted original native census: "
              ++ show (unitString (moduleUnit owner),moduleNameString (moduleName owner))))
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
        (hit,raw) <- timeModuleDetailPhase timing "prepared_graph" "raw_projection_task_service"
          (pmModule prepared) $
          projectCachedOriginalHomeModuleProducts completedRaw env selected context prepared
        let version = ownerVersion selected (pmModule prepared)
        report RawProjection (if hit then ReuseHit else ReuseMiss)
          (if hit then Matched else Recovery) version (if hit then Just 0 else Nothing)
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
        let pending = Set.toAscList (Set.fromList
              [owner | identity <- Set.toAscList (rawOriginalProductDemands raw `Set.difference` known)
                , let owner = mkModule
                        (stringToUnit (T.unpack (symbolUnit identity)))
                        (mkModuleName (T.unpack (symbolModule identity)))
                , toUnitId (moduleUnit owner) `Set.member` hsc_all_home_unit_ids env
                , owner `Set.notMember` sourceOwners
                , Map.notMember owner modules, owner `Set.notMember` attempted])
        modifyIORef' attemptedRef (`Set.union` Set.fromList pending)
        let siblings = Map.unions (frozenSiblings : map pmSitedSiblings (Map.elems modules))
        acquired <- fmap (Map.fromList . mapMaybe id) $ forM pending $ \owner -> do
          original <- acquireOriginal siblings owner
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
        _ <- runCompilerTasks executor
          (\(owner,task) -> do
            original <- runPreparedModuleTask task
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
        pure (modules,PreparedProductContext products admitted modules (Just raw) external Nothing)
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
        (Just completedRaw) (Just cache) executor reuse
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
newPreparedOriginalInterfaceArtifacts prepared directory =
  newOriginalInterfaceArtifactsWithReader readRetained (prHscEnv result)
    (pprFinalizedModules prepared) (retainedOriginalInterfaces prepared)
    (prInjectedSessionInterfaces result) (prProducedSessionInterfaces result) directory
  where
    result = pprPipelineResult prepared
    readRetained artifact = case compilationScope <$> preparedExactCompilation prepared of
      Just scope | artifact `elem` ([iface | (iface,_,_) <- scopeInterfaces scope] ++ scopeValueInterfaces scope) ->
        scopeInterfaceToken scope artifact
      _ -> case [preparedCandidateProof admission | admission <- pprCandidateAdmissions prepared
          , let candidate = preparedCandidateOriginal admission
          , (candidateUnit candidate,candidateModule candidate,candidateInterface candidate)
              == (exactUnit artifact,exactModule artifact,exactPath artifact)] of
        [proof] -> canonicalProofInterfaceBody proof artifact
        _ -> fail "retained interface lacks its prepared admission owner"

writeCertifiedProductsKeeping
  :: [FilePath] -> OriginalInterfaceArtifacts -> FilePath -> PreparedPipelineResult -> Maybe PreparedModuleProducts
  -> [(String, WireProgram)] -> IO CertifiedOriginalProducts
writeCertifiedProductsKeeping includes originalInterfaces outDir prepared productContext targets =
  writeCertifiedProductsKeepingWithOriginals includes originalInterfaces outDir prepared
    (fmap (\products -> PreparedProductContext products Map.empty (pprModules prepared) Nothing Set.empty Nothing) productContext) targets

writeCertifiedProductsKeepingWithOriginals
  :: [FilePath] -> OriginalInterfaceArtifacts -> FilePath -> PreparedPipelineResult -> Maybe PreparedProductContext
  -> [(String, WireProgram)] -> IO CertifiedOriginalProducts
writeCertifiedProductsKeepingWithOriginals includes originals directory prepared context targets =
  fst <$> writeCertifiedProducts OrdinaryProductFacts False includes originals directory prepared context targets

writeCertifiedSegmentProducts
  :: [FilePath] -> OriginalInterfaceArtifacts -> FilePath -> PreparedPipelineResult
  -> PreparedProductContext -> IO StagedOriginalProducts
writeCertifiedSegmentProducts includes originals directory prepared context = do
  (certified,bytes) <- writeCertifiedProducts SegmentOriginalFacts True includes originals directory prepared (Just context) []
  pure (StagedOriginalProducts certified (directory </> "certified-products.cbor") bytes)

writeCertifiedSegmentItemProducts
  :: PreparedPipelineResult -> StagedOriginalProducts -> FilePath -> WireProgram -> IO ()
writeCertifiedSegmentItemProducts prepared (StagedOriginalProducts originals _ _) directory program = do
  context <- maybe (fail "item output lacks its segment original certification") pure
    (certifiedTargetContext originals)
  certified <- encodeCertifiedItemProducts (prHscEnv (pprPipelineResult prepared)) context program
  bytes <- either fail pure certified
  BS.writeFile (directory </> "certified-products.cbor") bytes

publishStagedOriginalProducts :: PreparedPipelineResult -> StagedOriginalProducts -> IO CertifiedOriginalProducts
publishStagedOriginalProducts prepared (StagedOriginalProducts certified path bytes) = do
  revalidatePreparedCandidateInputs prepared
  let env = prHscEnv (pprPipelineResult prepared)
  case preparedExactCompilation prepared of
    Nothing -> BS.writeFile path bytes
    Just compilation -> writeCheckedExactCompilationWithPublication env compilation
      (preparedFreshDependencies prepared) (BS.writeFile path bytes)
  pure certified

writeCertifiedProducts
  :: CertifiedProductKind -> Bool
  -> [FilePath] -> OriginalInterfaceArtifacts -> FilePath -> PreparedPipelineResult -> Maybe PreparedProductContext
  -> [(String, WireProgram)] -> IO (CertifiedOriginalProducts, BS.ByteString)
writeCertifiedProducts kind stageCertificate includes originalInterfaces outDir prepared productContext targets = do
    let hscEnv = prHscEnv (pprPipelineResult prepared)
        retained = maybe Map.empty preparedRetainedOriginals productContext
        retainedProofs = Map.fromList
          [((unitString (moduleUnit owner),moduleNameString (moduleName owner)),admittedOriginalProof original)
          | (owner,original) <- Map.toAscList retained]
    timing <- readTimingEnabled
    let dependencies = preparedFreshDependencies prepared
    issued <- traverse (admitCurrentOriginalProducts originalInterfaces outDir prepared) productContext
    finalized <- case issued >>= preparedCurrentOriginalInventory of
      Just inventory -> materializeFinalizedModuleArtifacts outDir (currentOriginalFinalized inventory)
      Nothing -> timeDetailPhase timing "module_products" "capture_finalization" $
        captureFinalizedModuleArtifacts originalInterfaces hscEnv
          (pprFinalizedModules prepared) (pprPackageImports prepared) dependencies outDir
    let inventory = issued >>= preparedCurrentOriginalInventory
        availability = maybe Map.empty currentOriginalAvailability inventory
        freshProducts = maybe [] currentOriginalProducts inventory
        productPackages = maybe Map.empty currentOriginalPackages inventory
    productBytes <- timeDetailPhase timing "module_products" "write_products" $
      writeProductInventory outDir freshProducts
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
        finalDependencies = case preparedExactCompilation prepared of
          Nothing -> freshDependencies
          Just _ -> freshDependencies
            { dependencyCacheSafe = False, dependencySelectionComplete = False }
    timeDetailPhase timing "module_products" "dependency_evidence" $
      writeDependencyEvidence outDir finalDependencies
    evidenceBytes <- timeDetailPhase timing "module_products" "certificate_inputs" $
      BS.readFile (outDir </> "dependencies.json")
    sourceRecipe <- case preparedExactCompilation prepared of
      Nothing -> pure OrdinaryExecutionSource
      Just compilation -> issueFreshExecutionSource includes prepared freshDependencies
        [product | product <- freshProducts, let (unit,name,_,_) = moduleProductInput product
          , Map.notMember (T.unpack unit,T.unpack name) retainedProofs]
        (compilationScope compilation)
    case sourceRecipe of
      ExactExecutionSourceAvailable graph ->
        BS.writeFile (outDir </> "execution-source.cbor") (executionGraphBytes graph)
      _ -> pure ()
    (retainedVersions,targetContext,certificateBytes) <- timeDetailPhase timing "module_products" "certify" $ do
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
                        (T.unpack source) iface native packages)) (sourceProductSha256 finalDependencies (T.unpack unit) (T.unpack name))
                    _ -> ""]
      reconciled <- case inventory of
        Just current -> pure (currentReconciledOriginalProducts current)
        Nothing -> either (ioError . userError) pure (reconcileOriginalProducts
          (compilationScope <$> preparedExactCompilation prepared) [])
      certified <- encodeCertifiedOriginalProducts kind retainedProofs emittedSeals reconciled hscEnv sourceRecipe (pprProductInterfaces prepared) finalized (pprAcceptedCandidates prepared)
        (compilationScope <$> preparedExactCompilation prepared)
        (map moduleProductInput freshProducts) targets
        finalDependencies productBytes evidenceBytes
      case certified of
        Right (bytes,versions,context) -> do
          unless stageCertificate $ BS.writeFile (outDir </> "certified-products.cbor") bytes
          pure (versions,case kind of OrdinaryProductFacts -> Nothing; SegmentOriginalFacts -> Just context,bytes)
        Left reason -> do
          hPutStrLn stderr ("product certification unavailable: " ++ reason)
          unless stageCertificate $ BS.writeFile (outDir </> "certified-products.cbor") BS.empty
          unless (null freshProducts) $ fail ("native product certification failed: " ++ reason)
          pure (Map.empty,Nothing,BS.empty)
    sourceOriginals <- case preparedExactCompilation prepared of
      Nothing -> pure Map.empty
      Just compilation -> do
        let owner = tcg_mod (prTargetTcGblEnv (pprPipelineResult prepared))
        captureFinalizedSourceOriginals compilation (pprAcceptedCandidates prepared)
          (unitString (moduleUnit owner),moduleNameString (moduleName owner)) finalized finalDependencies
    unless stageCertificate $ do
      revalidatePreparedCandidateInputs prepared
      forM_ (preparedExactCompilation prepared) $ \compilation ->
        writeCheckedExactCompilation hscEnv compilation freshDependencies
    pure (CertifiedOriginalProducts freshProducts sourceOriginals finalized sourceRecipe retainedProofs retainedVersions targetContext, certificateBytes)


-- An immutable original product needs captured finalized Core as well as its
-- interface. Unsupported Core remains interface-only; native target projection
-- and required global ownership still enforce their own complete contracts.
admitModuleProducts :: OriginalInterfaceArtifacts -> PreparedProductContext
  -> Map.Map (String,String) LocalFinalizedAdmission
  -> Map.Map ModuleName ModIface
  -> Map.Map ModuleName PackageImportEvidence
  -> IO (Map.Map (String, String) ProductAvailability,
         [ModuleProductEncoding], Map.Map (String,String) BS.ByteString)
admitModuleProducts originalInterfaces productContext finalized interfaces packageRoots = do
  let inventory = preparedProductInventory productContext
  timing <- readTimingEnabled
  forM_ (preparedModuleProductOmissions inventory) $ \(owner, omissions) ->
    forM_ omissions $ \omission ->
      hPutStrLn stderr ("module product group omitted: " ++ unitString (moduleUnit owner) ++ ":" ++ moduleNameString (moduleName owner)
        ++ "#" ++ show (omittedOriginalOrdinal omission) ++ " "
        ++ show (omittedOriginalBinders omission) ++ " "
        ++ show (omittedOriginalReason omission))
  outcomes <- forM (preparedModuleProductOutcomes inventory) $ \(owner, outcome) -> do
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
          Nothing -> fail "fresh original product lacks its captured finalization"
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
          let iface = ExactIfaceArtifact (fst key) (snd key) "" (shaHex bytes) []
          pure (encodePackageImports iface roots)
      when (BS.length sidecar > 4 * 1024 * 1024) $
        ioError (userError "direct package import witness exceeds four MiB")
      let retainedGroups = do
            raw <- preparedRawProducts productContext
            ownerRaw <- case filter ((== owner) . rawOriginalProductOwner) raw of
              [selected] -> Just selected
              _ -> Nothing
            traverse (\group -> Map.lookup (projectedOriginalOrdinal group)
              (rawOriginalGroupEncodings ownerRaw)) groups
          encoded = case retainedGroups of
            Just encodedGroups -> prepareModuleProductEncodingFromGroups
              (T.pack (fst key)) (T.pack (snd key)) bytes encodedGroups
            Nothing -> prepareModuleProductEncoding (T.pack (fst key),T.pack (snd key),bytes,groups)
      pure (key, ProductReady, Just encoded, Just sidecar)

-- Interface-only captures emit the same valid inventory framing with no native
-- rows. Product absence never prevents retaining the actual finalization.
writeProductInventory :: FilePath -> [ModuleProductEncoding] -> [(String,String,BS.ByteString)] -> IO BS.ByteString
writeProductInventory outDir products packageBundles = do
  timing <- readTimingEnabled
  let productBytes = encodeModuleProductInventory products
  timeDetailPhase timing "module_products" "encode_products" $
    BS.writeFile (outDir </> "module-products.cbor") productBytes
  timeDetailPhase timing "module_products" "encode_package_bundles" $
    BS.writeFile (outDir </> "module-package-imports.cbor")
    (toStrictByteString (encodeListLen 3
      <> encodeString (T.pack "TPPKGBUNDLES") <> encodeWord 1
      <> encodeListLen (fromIntegral (length packageBundles))
      <> foldMap (\(unit, moduleName', sidecar) -> encodeListLen 3
        <> encodeString (T.pack unit) <> encodeString (T.pack moduleName')
        <> encodeBytes sidecar) packageBundles))
  pure productBytes


-- A later item can execute a quoter defined by an original retained here.
-- Keep its consumed source recipe at the same boundary as its native product,
-- rather than waiting for the frontend's postworker item certification.
issueFreshExecutionSource
  :: [FilePath] -> PreparedPipelineResult -> DependencyEvidence -> [ModuleProductEncoding]
  -> ExactScope -> IO WorkerExecutionSource
issueFreshExecutionSource includes prepared evidence fullProducts scope
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
              retained = map originalIdentity (scopeProducts scope)
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
retainStagedProgramProducts directory prepared staged target initial =
  retainProgramProductsWithPublication
    (Just (stagedCertificatePath staged,stagedCertificateBytes staged))
    directory prepared (stagedCertifiedOriginalProducts staged) target initial

stagedCertificatePath :: StagedOriginalProducts -> FilePath
stagedCertificatePath (StagedOriginalProducts _ path _) = path

stagedCertificateBytes :: StagedOriginalProducts -> BS.ByteString
stagedCertificateBytes (StagedOriginalProducts _ _ bytes) = bytes

retainProgramProductsWithPublication
  :: Maybe (FilePath,BS.ByteString) -> FilePath -> PreparedPipelineResult
  -> CertifiedOriginalProducts -> String -> ExactScope -> IO ExactScope
retainProgramProductsWithPublication stagedCertificate directory prepared certified target initial = do
  selected <- either fail pure (extendSourceSelectedOriginals
    (preparedExactCompilation prepared >>= compilationSourceSelection) initial)
  finalized <- foldM retainInterface (selected,[],[],[]) (Map.toAscList localInterfaces)
  (cached,additions,pendingProducts,pendingLexical) <- foldM retainCached finalized (zip [0::Int ..] (pprCandidateAdmissions prepared))
  admitted <- extendExactScopeGeneration cached additions pendingProducts pendingLexical >>= either fail pure
  promoted <- foldM retain admitted (zip [0::Int ..] products)
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
    OrdinaryExecutionSource -> fail "exact program products lack exact source recipe outcome"
  revalidatePreparedCandidateInputs prepared
  let env = prHscEnv (pprPipelineResult prepared)
  let publishStaged = forM_ stagedCertificate $ \(path,bytes) -> BS.writeFile path bytes
  case preparedExactCompilation prepared of
    Nothing -> do
      revalidateExactScopesAt RetainedProductsPublication env [retained] >>= either fail pure
      publishStaged
    Just compilation -> case stagedCertificate of
      Nothing -> writeRetainedExactCompilation env retained compilation
        (preparedFreshDependencies prepared)
      Just (path,bytes) -> writeRetainedExactCompilationWithPublication env retained compilation
        (preparedFreshDependencies prepared) (BS.writeFile path bytes)
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
    retainCached (scope,additions,stagedProducts,stagedLexical) (index, admission) = do
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
          BS.writeFile interfacePath interfaceBytes
          BS.writeFile packagesPath packageBytes
          BS.writeFile productPath productBytes
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

programSourceRequirements :: PreparedPipelineResult -> String -> String -> IO [(String, String)]
programSourceRequirements prepared unit owner =
  either fail pure (preparedHomeRequirements prepared unit owner)
