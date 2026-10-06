{-# LANGUAGE OverloadedStrings #-}

-- | Emit the compiler's complete original-product result once. Capture paths
-- belong to the supplied output directory; original source identities and
-- independently admitted candidate/exact evidence remain compiler inputs.
module Tidepool.CompilerProducts
  ( CertifiedOriginalProducts, certifiedOriginalProducts, certifiedFinalizedArtifacts
  , certifiedSourceOriginals, certifiedExecutionSource, writeCertifiedProductsKeeping, retainedOriginalInterfaces
  , certifiedRetainedOriginals, certifiedRetainedNativeVersions, PreparedProductContext, prepareOriginalProducts, prepareOriginalProductsWithExecutor
  , requireOriginalExecutableGlobals
  , writeCertifiedProductsKeepingWithOriginals
  , prepareCompilerProjectionContext, prepareCompilerProjectionContextForEnvironment, exactProgramProductVersionFromDigest
  , OriginalProjectionCollector, newOriginalProjectionCollector, observeOriginalProjection
  , prepareOriginalProductsWithCollector
  , CurrentOriginalInventory, admitCurrentOriginalProducts, preparedCurrentOriginalInventory
  , preparedProductInventory, currentOriginalBinders, currentOriginalBindingsExcept
  ) where

import Codec.CBOR.Encoding
import Codec.CBOR.Write (toStrictByteString)
import Control.Applicative ((<|>))
import Control.Exception (throwIO, evaluate)
import Control.Concurrent.MVar (MVar, newMVar, modifyMVar_, readMVar)
import Control.Monad (foldM, forM, forM_, unless, when)
import Data.Bits (shiftR)
import Data.ByteString qualified as BS
import Data.IORef (newIORef, readIORef, modifyIORef')
import Data.Map.Strict qualified as Map
import Data.Maybe (mapMaybe, isJust)
import Data.Set qualified as Set
import Data.Text qualified as T
import Data.Text.Encoding qualified as TE
import Data.Word (Word64)
import GHC.Tc.Types (tcg_mod)
import GHC.Types.Name (Name)
import GHC.Driver.Env (HscEnv, hsc_all_home_unit_ids)
import GHC.Unit.Module (Module, ModuleName, moduleName, moduleNameString, moduleUnit, mkModuleName, mkModule)
import GHC.Unit.Module.ModIface (ModIface, mi_module)
import GHC.Unit.Types (unitString, stringToUnit, toUnitId)
import Numeric (readHex)
import System.Directory (canonicalizePath, doesPathExist, makeAbsolute)
import System.FilePath (normalise, (</>))
import System.IO (hPutStrLn, stderr)
import System.Mem.StableName (StableName, makeStableName)
import System.Info qualified as SystemInfo
import Tidepool.CertifiedProducts (encodeCertifiedProductsWithOriginals, sourceProductSha256)
import Tidepool.DependencyEvidence
import Tidepool.ExactHydration
  ( OriginalInterfaceArtifacts, ExactIfaceArtifact(..), originalInterfaceBytes )
import Tidepool.ExactScope
  ( ExactScope(..), ExactCompilation(..), ExactProduct(..), scopeValueInterfaces
  , revalidateExactScope, writeExactCompilation, scopeCanonicalInterfaces
  , CanonicalInterfaceProof, captureFinalizedSourceOriginals )
import Tidepool.ExecutionEncode
  ( ModuleProductEncoding, moduleProductInput, moduleProductBytes
  , prepareModuleProductEncoding, encodeModuleProductInventory )
import Tidepool.ExecutionProjection
  ( ProjectionContext(..), ProjectionError(..), PreparedModuleProducts, OriginalGroupOmission(..)
  , preparedModuleProductOutcomes, preparedModuleProductOmissions, resolveTextPackageUnit
  , projectRawOriginalHomeModuleProducts, forceRawModuleProducts, rawOriginalProductOwner
  , rawOriginalProductBinders, rawOriginalProductDemands
  , settleOriginalHomeModuleProducts, settleOriginalHomeModuleProductsWithoutOwners
  , RawModuleProducts, preparedTopIdentityBindings )
import Tidepool.ExecutionSchema
  ( Architecture(..), Endianness(..), SymbolIdentity(..), TargetDescriptor(..), WireProgram
  , GlobalDecl(..), ProjectedGroup(..) )
import Tidepool.ExecutionSource
  ( WorkerExecutionSource(..), SourceRecipeUnavailable(..), ExecutionSourceRecipe(..)
  , ExecutionSourceGraph(..), ExecutionSourceIdentity(..), ExecutionSourceOwner(..)
  , ExecutionSourceFailure(..), executionIdentityKey, issueExecutionSourceRecipe
  , executionSourceInheritedOwners )
import Tidepool.ExtractUtil (shaHex)
import Tidepool.FinalizedModuleArtifacts
  ( FinalizedModuleArtifacts, captureFinalizedModuleArtifacts, LocalFinalizedAdmission
  , finalizedLocalAdmissions, localFinalizedCore )
import Tidepool.GhcPipeline
  ( PreparedPipelineResult(..), PipelineResult(..), preparedFreshDependencies, preparedExactCompilation )
import Tidepool.ModuleCandidates (ModuleCandidate(..), candidateExecutionSources)
import Tidepool.PackageWitness (PackageImportEvidence(..), PackageImportRoot(..), encodePackageImports)
import Tidepool.PreparedFormatting (resolveFormattingAuthority)
import Tidepool.PreparedJson (JsonAuthority, resolveJsonAuthorityWithCanonicalInterfaces)
import Tidepool.PreparedTime (resolveTimeAuthority)
import Tidepool.PreparedStg (PreparedModule, pmModule, pmSitedSiblings, acquirePreparedModule, runPreparedModuleTask)
import Tidepool.CompilerExecution (CompilerExecutor, withCompilerExecutor, serialCompilerExecutionGrant, runCompilerTasks)
import Tidepool.HomeProducts
  ( AdmittedFinalizedOriginal, recoverAdmittedFinalizedOriginal, admittedOriginalModule
  , admittedOriginalProof, admittedOriginalInterface, admittedOriginalLocation )
import Tidepool.FinalizedModule (finalizedHomeModInfo)
import GHC.Unit.Home.ModInfo (hm_iface)
import Tidepool.Timing (readTimingEnabled, timePhase, timeDetailPhase)

-- The production worker and original-product fixtures share the compiler
-- profile and native package/type authorities. Callers select only their real
-- root/target and independently admitted host inputs.
prepareCompilerProjectionContext
  :: PreparedPipelineResult -> Map.Map SymbolIdentity Word64 -> Module -> String -> [String]
  -> Maybe JsonAuthority -> IO ProjectionContext
prepareCompilerProjectionContext prepared retainedGenerations owner target auxiliaryRoots hostJsonAuthority =
  prepareCompilerProjectionContextForEnvironment
    (prHscEnv (pprPipelineResult prepared))
    (compilationScope <$> preparedExactCompilation prepared)
    retainedGenerations owner target auxiliaryRoots hostJsonAuthority

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
newtype OriginalProjectionCollector = OriginalProjectionCollector
  (MVar (Map.Map Module (StableName PreparedModule, ProjectionContext, RawModuleProducts)))

newOriginalProjectionCollector :: IO OriginalProjectionCollector
newOriginalProjectionCollector = OriginalProjectionCollector <$> newMVar Map.empty

-- The pipeline acquires authorities on its coordinator before tasks start.
-- The returned observer runs inside each admitted native task, then publishes
-- immutable results under a short lock; it does no live Session acquisition.
observeOriginalProjection :: OriginalProjectionCollector
  -> Map.Map SymbolIdentity Word64 -> [String] -> Maybe JsonAuthority
  -> HscEnv -> Map.Map ModuleName ModIface -> Module -> Maybe ExactScope
  -> IO (PreparedModule -> IO ())
observeOriginalProjection (OriginalProjectionCollector completed) retained auxiliaryRoots json
    environment interfaces owner exact = do
  context <- prepareCompilerProjectionContextForEnvironment environment exact retained owner
    "__original_projection" auxiliaryRoots json
  pure $ \prepared -> do
    raw <- forceRawModuleProducts (projectRawOriginalHomeModuleProducts environment interfaces context prepared)
    identity <- evaluate prepared >>= makeStableName
    let selectedContext = originalProjectionContext prepared context
    modifyMVar_ completed (pure . Map.insert (pmModule prepared) (identity,selectedContext,raw))

originalProjectionContext :: PreparedModule -> ProjectionContext -> ProjectionContext
originalProjectionContext prepared context = context
  { projectionEntry = SymbolIdentity "" "" "value" "" Nothing
  , projectionAuxiliaryRoots = Set.toAscList (Set.fromList (projectionAuxiliaryRoots context)
      `Set.intersection` Set.fromList (Map.elems (preparedTopIdentityBindings [prepared])))
  }

data CertifiedOriginalProducts = CertifiedOriginalProducts
  { certifiedOriginalProducts :: [ModuleProductEncoding]
  , certifiedSourceOriginals :: Map.Map (String,String) CanonicalInterfaceProof
  , certifiedFinalizedArtifacts :: FinalizedModuleArtifacts
  , certifiedExecutionSource :: WorkerExecutionSource
  , certifiedRetainedOriginals :: Map.Map (String,String) CanonicalInterfaceProof
  , certifiedRetainedNativeVersions :: Map.Map (String,String) String
  }

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
  , currentOriginalNames :: Map.Map Name SymbolIdentity
  }

currentOriginalBinders :: CurrentOriginalInventory -> Set.Set SymbolIdentity
currentOriginalBinders inventory = Set.fromList
  [binder | product' <- currentOriginalProducts inventory
    , let (_,_,_,groups) = moduleProductInput product'
    , group <- groups, binder <- projectedBinders group]

-- Entries and auxiliary roots retain their complete original recursive group.
-- Every remaining group crosses the executable boundary through its exact Name.
currentOriginalBindingsExcept :: CurrentOriginalInventory -> Set.Set SymbolIdentity
  -> Map.Map Name SymbolIdentity
currentOriginalBindingsExcept inventory roots = Map.filter (`Set.member` imported)
  (currentOriginalNames inventory)
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
            Just raw -> productContext {preparedProductInventory = fst
              (settleOriginalHomeModuleProductsWithoutOwners environment
                (preparedExternalBinders productContext) withheld raw)}
      (availability,products,packages) <- admitModuleProducts originalInterfaces admittedContext
        (finalizedLocalAdmissions finalized) (pprProductInterfaces prepared)
        (pprPackageImports prepared)
      let inventory = CurrentOriginalInventory availability products packages finalized
            (preparedTopIdentityBindings (preparedProductModules productContext))
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
prepareOriginalProductsWithExecutor = prepareOriginalProductsUsingCollector Nothing

prepareOriginalProductsWithCollector
  :: OriginalProjectionCollector -> CompilerExecutor -> HscEnv -> Maybe ExactScope
  -> Map.Map ModuleName ModIface -> ProjectionContext -> Set.Set SymbolIdentity -> [PreparedModule]
  -> IO ([PreparedModule], PreparedProductContext)
prepareOriginalProductsWithCollector collector = prepareOriginalProductsUsingCollector (Just collector)

prepareOriginalProductsUsingCollector
  :: Maybe OriginalProjectionCollector -> CompilerExecutor -> HscEnv -> Maybe ExactScope
  -> Map.Map ModuleName ModIface -> ProjectionContext -> Set.Set SymbolIdentity -> [PreparedModule]
  -> IO ([PreparedModule], PreparedProductContext)
prepareOriginalProductsUsingCollector collector executor env exact interfaces context external initial = do
  seeds <- case collector of
    Nothing -> pure Map.empty
    Just (OriginalProjectionCollector completed) -> readMVar completed
  modulesRef <- newIORef (Map.fromList [(pmModule prepared, prepared) | prepared <- initial])
  admittedRef <- newIORef Map.empty
  attemptedRef <- newIORef Set.empty
  rawRef <- newIORef Map.empty
  knownRef <- newIORef (Set.union external (Set.fromList
    (Map.elems (preparedTopIdentityBindings initial))))
  let lower selected prepared = do
        identity <- evaluate prepared >>= makeStableName
        case Map.lookup (pmModule prepared) seeds of
          Just (capturedIdentity,capturedContext,raw)
            | identity == capturedIdentity
            , originalProjectionContext prepared context == capturedContext -> pure raw
          _ -> forceRawModuleProducts
            (projectRawOriginalHomeModuleProducts env selected context prepared)
      completed _ raw = do
        modifyIORef' rawRef (Map.insert (rawOriginalProductOwner raw) raw)
        modules <- readIORef modulesRef
        attempted <- readIORef attemptedRef
        known <- readIORef knownRef
        let pending = Set.toAscList (Set.fromList
              [owner | identity <- Set.toAscList (rawOriginalProductDemands raw `Set.difference` known)
                , let owner = mkModule (stringToUnit (T.unpack (symbolUnit identity)))
                      (mkModuleName (T.unpack (symbolModule identity)))
                , toUnitId (moduleUnit owner) `Set.member` hsc_all_home_unit_ids env
                , Map.notMember owner modules, owner `Set.notMember` attempted])
        modifyIORef' attemptedRef (`Set.union` Set.fromList pending)
        originals <- fmap (Map.fromList . mapMaybe id) $ forM pending $ \owner -> case exact of
          Nothing -> pure Nothing
          Just scope -> fmap (fmap (\original -> (owner,original)))
            (recoverAdmittedFinalizedOriginal env scope owner)
        admitted <- readIORef admittedRef
        let allAdmitted = Map.union admitted originals
            siblings = Map.unions (map pmSitedSiblings (Map.elems modules))
            selected = Map.union interfaces (Map.fromList
              [(moduleName owner,hm_iface (finalizedHomeModInfo (admittedOriginalModule original)))
                | (owner,original) <- Map.toAscList allAdmitted])
        modifyIORef' admittedRef (Map.union originals)
        tasks <- forM (Map.toAscList originals) $ \(owner,original) -> do
          task <- acquirePreparedModule env (admittedOriginalLocation original) siblings
            (admittedOriginalModule original)
          pure (owner,task)
        _ <- runCompilerTasks executor
          (\(_,task) -> do
            prepared <- runPreparedModuleTask task
            product' <- lower selected prepared
            pure (prepared,product'))
          (\_ (prepared,product') -> do
            modifyIORef' modulesRef (Map.insert (pmModule prepared) prepared)
            modifyIORef' knownRef (`Set.union` Set.fromList
              (Map.elems (preparedTopIdentityBindings [prepared])))
            completed prepared product') tasks
        pure ()
  _ <- runCompilerTasks executor (lower interfaces) completed initial
  modules <- Map.elems <$> readIORef modulesRef
  admitted <- readIORef admittedRef
  raw <- Map.elems <$> readIORef rawRef
  let (products,_) = settleOriginalHomeModuleProducts env external raw
  pure (modules,PreparedProductContext products admitted modules (Just raw) external Nothing)

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

writeCertifiedProductsKeeping
  :: [FilePath] -> OriginalInterfaceArtifacts -> FilePath -> PreparedPipelineResult -> Maybe PreparedModuleProducts
  -> [(String, WireProgram)] -> IO CertifiedOriginalProducts
writeCertifiedProductsKeeping includes originalInterfaces outDir prepared productContext targets =
  writeCertifiedProductsKeepingWithOriginals includes originalInterfaces outDir prepared
    (fmap (\products -> PreparedProductContext products Map.empty (pprModules prepared) Nothing Set.empty Nothing) productContext) targets

writeCertifiedProductsKeepingWithOriginals
  :: [FilePath] -> OriginalInterfaceArtifacts -> FilePath -> PreparedPipelineResult -> Maybe PreparedProductContext
  -> [(String, WireProgram)] -> IO CertifiedOriginalProducts
writeCertifiedProductsKeepingWithOriginals includes originalInterfaces outDir prepared productContext targets = do
    let hscEnv = prHscEnv (pprPipelineResult prepared)
        retained = maybe Map.empty preparedRetainedOriginals productContext
        retainedProofs = Map.fromList
          [((unitString (moduleUnit owner),moduleNameString (moduleName owner)),admittedOriginalProof original)
          | (owner,original) <- Map.toAscList retained]
    timing <- readTimingEnabled
    let dependencies = preparedFreshDependencies prepared
    issued <- traverse (admitCurrentOriginalProducts originalInterfaces outDir prepared) productContext
    finalized <- case issued >>= preparedCurrentOriginalInventory of
      Just inventory -> pure (currentOriginalFinalized inventory)
      Nothing -> timeDetailPhase timing "module_products" "capture_finalization" $
        captureFinalizedModuleArtifacts originalInterfaces hscEnv
          (pprFinalizedModules prepared) (pprPackageImports prepared) dependencies outDir
    let inventory = issued >>= preparedCurrentOriginalInventory
        availability = maybe Map.empty currentOriginalAvailability inventory
        freshProducts = maybe [] currentOriginalProducts inventory
        productPackages = maybe Map.empty currentOriginalPackages inventory
    timeDetailPhase timing "module_products" "write_products" $
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
    forM_ (preparedExactCompilation prepared) $ \compilation -> do
      verified <- revalidateExactScope hscEnv (compilationScope compilation)
      either (ioError . userError) pure verified
      writeExactCompilation compilation freshDependencies
    (productBytes, evidenceBytes) <- timeDetailPhase timing "module_products" "certificate_inputs" $ do
      productBytes <- BS.readFile (outDir </> "module-products.cbor")
      evidenceBytes <- BS.readFile (outDir </> "dependencies.json")
      pure (productBytes, evidenceBytes)
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
    retainedVersions <- timeDetailPhase timing "module_products" "certify" $ do
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
      certified <- encodeCertifiedProductsWithOriginals retainedProofs emittedSeals hscEnv sourceRecipe (pprProductInterfaces prepared) finalized (pprAcceptedCandidates prepared)
        (compilationScope <$> preparedExactCompilation prepared)
        (map moduleProductInput freshProducts) targets
        finalDependencies productBytes evidenceBytes
      versions <- case certified of
        Right (bytes,versions) -> do
          BS.writeFile (outDir </> "certified-products.cbor") bytes
          pure versions
        Left reason -> do
          hPutStrLn stderr ("product certification unavailable: " ++ reason)
          BS.writeFile (outDir </> "certified-products.cbor") BS.empty
          unless (null freshProducts) $ fail ("native product certification failed: " ++ reason)
          pure Map.empty
      pure versions
    sourceOriginals <- case preparedExactCompilation prepared of
      Nothing -> pure Map.empty
      Just compilation -> do
        let owner = tcg_mod (prTargetTcGblEnv (pprPipelineResult prepared))
        captureFinalizedSourceOriginals compilation (pprAcceptedCandidates prepared)
          (unitString (moduleUnit owner),moduleNameString (moduleName owner)) finalized finalDependencies
    pure (CertifiedOriginalProducts freshProducts sourceOriginals finalized sourceRecipe retainedProofs retainedVersions)


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
      retained = preparedRetainedOriginals productContext
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
    issue timing key name owner groups = do
      bytes <- timeDetailPhase timing "module_products.interfaces" (snd key) $
        originalInterfaceBytes originalInterfaces owner
          >>= maybe (fail "original product interface lacks its captured artifact") pure
      sidecar <- case Map.lookup owner retained of
        Just original -> do
          let (iface,path,seal) = admittedOriginalInterface original
          captured <- BS.readFile path
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
      pure (key, ProductReady, Just (prepareModuleProductEncoding (T.pack (fst key),
        T.pack (snd key), bytes, groups)), Just sidecar)

-- Interface-only captures emit the same valid inventory framing with no native
-- rows. Product absence never prevents retaining the actual finalization.
writeProductInventory :: FilePath -> [ModuleProductEncoding] -> [(String,String,BS.ByteString)] -> IO ()
writeProductInventory outDir products packageBundles = do
  timing <- readTimingEnabled
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
