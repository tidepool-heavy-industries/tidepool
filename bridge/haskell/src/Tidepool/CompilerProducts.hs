{-# LANGUAGE OverloadedStrings #-}

-- | Emit the compiler's complete original-product result once. Capture paths
-- belong to the supplied output directory; original source identities and
-- independently admitted candidate/exact evidence remain compiler inputs.
module Tidepool.CompilerProducts
  ( CertifiedOriginalProducts, certifiedOriginalProducts, certifiedFinalizedArtifacts
  , certifiedExecutionSource, writeCertifiedProductsKeeping, retainedOriginalInterfaces
  , prepareCompilerProjectionContext, exactProgramProductVersionFromDigest
  ) where

import Codec.CBOR.Encoding
import Codec.CBOR.Write (toStrictByteString)
import Control.Exception (throwIO)
import Control.Monad (foldM, forM, forM_, unless, when)
import Data.Bits (shiftR)
import Data.ByteString qualified as BS
import Data.Map.Strict qualified as Map
import Data.Maybe (mapMaybe)
import Data.Set qualified as Set
import Data.Text qualified as T
import Data.Text.Encoding qualified as TE
import Data.Word (Word64)
import GHC.Unit.Module (Module, ModuleName, moduleName, moduleNameString, moduleUnit, mkModuleName)
import GHC.Unit.Module.ModIface (ModIface, mi_module)
import GHC.Unit.Types (unitString)
import Numeric (readHex)
import System.Directory (canonicalizePath, doesPathExist, makeAbsolute)
import System.FilePath (normalise, (</>))
import System.IO (hPutStrLn, stderr)
import System.Info qualified as SystemInfo
import Tidepool.CertifiedProducts (encodeCertifiedProducts)
import Tidepool.DependencyEvidence
import Tidepool.ExactHydration
  ( OriginalInterfaceArtifacts, ExactIfaceArtifact(..), originalInterfaceBytes )
import Tidepool.ExactScope
  ( ExactScope(..), ExactCompilation(..), ExactProduct(..), scopeValueInterfaces
  , revalidateExactScope, writeExactCompilation, scopeCanonicalInterfaces )
import Tidepool.ExecutionEncode
  ( ModuleProductEncoding, moduleProductInput, moduleProductBytes
  , prepareModuleProductEncoding, encodeModuleProductInventory )
import Tidepool.ExecutionProjection
  ( ProjectionContext(..), PreparedModuleProducts, OriginalGroupOmission(..)
  , preparedModuleProductOutcomes, preparedModuleProductOmissions, resolveTextPackageUnit )
import Tidepool.ExecutionSchema
  ( Architecture(..), Endianness(..), SymbolIdentity(..), TargetDescriptor(..), WireProgram )
import Tidepool.ExecutionSource
  ( WorkerExecutionSource(..), SourceRecipeUnavailable(..), ExecutionSourceRecipe(..)
  , ExecutionSourceGraph(..), ExecutionSourceIdentity(..), ExecutionSourceOwner(..)
  , ExecutionSourceFailure(..), executionIdentityKey, issueExecutionSourceRecipe
  , executionSourceInheritedOwners )
import Tidepool.ExtractUtil (shaHex)
import Tidepool.FinalizedModuleArtifacts (FinalizedModuleArtifacts, captureFinalizedModuleArtifacts)
import Tidepool.GhcPipeline (PreparedPipelineResult(..), PipelineResult(..))
import Tidepool.ModuleCandidates (ModuleCandidate(..), candidateExecutionSources)
import Tidepool.PackageWitness (PackageImportEvidence(..), PackageImportRoot(..), encodePackageImports)
import Tidepool.PreparedFormatting (resolveFormattingAuthority)
import Tidepool.PreparedJson (JsonAuthority, resolveJsonAuthorityWithCanonicalInterfaces)
import Tidepool.PreparedTime (resolveTimeAuthority)
import Tidepool.Timing (readTimingEnabled, timePhase, timeDetailPhase)

-- The production worker and original-product fixtures share the compiler
-- profile and native package/type authorities. Callers select only their real
-- root/target and independently admitted host inputs.
prepareCompilerProjectionContext
  :: PreparedPipelineResult -> Map.Map SymbolIdentity Word64 -> Module -> String -> [String]
  -> Maybe JsonAuthority -> IO ProjectionContext
prepareCompilerProjectionContext prepared retainedGenerations owner target auxiliaryRoots hostJsonAuthority = do
  timing <- readTimingEnabled
  let environment = prHscEnv (pprPipelineResult prepared)
      exactScope = compilationScope <$> pprExactCompilation prepared
      symbol occurrence = SymbolIdentity (T.pack (unitString (moduleUnit owner)))
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
    , projectionEntry = symbol target
    , projectionAuxiliaryRoots = map symbol auxiliaryRoots
    , projectionFormattingAuthority = formattingAuthority
    , projectionTimeAuthority = timeAuthority
    , projectionJsonAuthority = jsonAuthority
    , projectionTextUnit = textAuthority
    }

data CertifiedOriginalProducts = CertifiedOriginalProducts
  { certifiedOriginalProducts :: [ModuleProductEncoding]
  , certifiedFinalizedArtifacts :: FinalizedModuleArtifacts
  , certifiedExecutionSource :: WorkerExecutionSource
  }

-- Captures come from the exact scope, including its admitted checked values,
-- or the candidate owner. Their original bytes supply type dependency seals;
-- this lookup does not turn checked values into source finalizations or native
-- execution recipes.
retainedOriginalInterfaces :: PreparedPipelineResult -> [ExactIfaceArtifact]
retainedOriginalInterfaces prepared =
  [artifact | scope <- maybe [] pure (compilationScope <$> pprExactCompilation prepared)
    , (artifact, _, _) <- scopeInterfaces scope]
  ++ [artifact | scope <- maybe [] pure (compilationScope <$> pprExactCompilation prepared)
    , artifact <- scopeValueInterfaces scope]
  ++ [ExactIfaceArtifact (candidateUnit candidate) (candidateModule candidate)
        (candidateInterface candidate) (candidateInterfaceSha256 candidate)
        (candidateInterfaceRequirements candidate)
     | candidate <- pprAcceptedCandidates prepared]

writeCertifiedProductsKeeping
  :: [FilePath] -> OriginalInterfaceArtifacts -> FilePath -> PreparedPipelineResult -> Maybe PreparedModuleProducts
  -> [(String, WireProgram)] -> IO CertifiedOriginalProducts
writeCertifiedProductsKeeping includes originalInterfaces outDir prepared productContext targets = do
    let hscEnv = prHscEnv (pprPipelineResult prepared)
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
    sourceRecipe <- case pprExactCompilation prepared of
      Nothing -> pure OrdinaryExecutionSource
      Just compilation -> issueFreshExecutionSource includes prepared freshDependencies freshProducts
        (compilationScope compilation)
    case sourceRecipe of
      ExactExecutionSourceAvailable graph ->
        BS.writeFile (outDir </> "execution-source.cbor") (executionGraphBytes graph)
      _ -> pure ()
    finalized <- timeDetailPhase timing "module_products" "certify" $ do
      finalized <- captureFinalizedModuleArtifacts originalInterfaces hscEnv
        (pprFinalizedModules prepared) (pprPackageImports prepared) finalDependencies outDir
      certified <- encodeCertifiedProducts hscEnv sourceRecipe (pprProductInterfaces prepared) finalized (pprAcceptedCandidates prepared)
        (compilationScope <$> pprExactCompilation prepared)
        (map moduleProductInput freshProducts) targets
        finalDependencies productBytes evidenceBytes
      case certified of
        Right bytes -> BS.writeFile (outDir </> "certified-products.cbor") bytes
        Left reason -> do
          hPutStrLn stderr ("product certification unavailable: " ++ reason)
          BS.writeFile (outDir </> "certified-products.cbor") BS.empty
          unless (null freshProducts) $ fail ("native product certification failed: " ++ reason)
      pure finalized
    pure (CertifiedOriginalProducts freshProducts finalized sourceRecipe)


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
  forM_ (preparedModuleProductOmissions inventory) $ \(owner, omissions) ->
    forM_ omissions $ \omission ->
      hPutStrLn stderr ("module product group omitted: " ++ unitString (moduleUnit owner) ++ ":" ++ moduleNameString (moduleName owner)
        ++ "#" ++ show (omittedOriginalOrdinal omission) ++ " "
        ++ show (omittedOriginalBinders omission) ++ " "
        ++ show (omittedOriginalReason omission))
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
        (pprExactCompilation prepared)
      let exactRows = compilationImports compilation
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

