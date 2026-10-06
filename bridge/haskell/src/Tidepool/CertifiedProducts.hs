{-# LANGUAGE OverloadedStrings #-}

module Tidepool.CertifiedProducts
  ( encodeCertifiedProducts, encodeCertifiedProductsWithOriginals
  , resolvePackageGlobal, homeInterfaceUsageOwners, sourceProductSha256 ) where

import Prelude hiding (product)
import Codec.CBOR.Encoding
  ( Encoding, encodeBool, encodeListLen, encodeNull, encodeString, encodeWord
  , encodeWord64 )
import Codec.CBOR.Write (toStrictByteString)
import Control.Exception (IOException, try)
import Control.Monad (forM, when)
import qualified Crypto.Hash.SHA256 as SHA256
import qualified Data.ByteString as BS
import Data.Foldable (fold)
import Data.Bits (shiftR)
import Data.List (find, mapAccumL)
import qualified Data.Map.Strict as Map
import Data.Maybe (catMaybes)
import qualified Data.Set as Set
import Data.IORef (IORef, newIORef, modifyIORef', readIORef)
import qualified Data.Text as T
import qualified Data.Text.Encoding as TE
import Data.Word (Word64)
import GHC.Driver.Env (HscEnv, hsc_all_home_unit_ids)
import GHC.Unit.Module (Module, ModuleName, mkModule, mkModuleName, moduleUnit, moduleName)
import GHC.Unit.Module.ModIface (ModIface, mi_decls, mi_exports, mi_module)
import GHC.Unit.Module.Location (ml_hi_file)
import GHC.Unit.Types (stringToUnit, toUnitId)
import GHC.Iface.Syntax (IfaceDecl(..), ifaceDeclImplicitBndrs)
import GHC.Iface.Load (importDecl)
import GHC.Tc.Utils.Monad (initIfaceLoad)
import GHC.Types.Id (Id)
import GHC.Types.Name (Name, getName, nameModule_maybe, nameOccName, wiredInNameTyThing_maybe)
import GHC.Types.Name.Occurrence (OccName, mkVarOcc, isVarOcc)
import GHC.Types.Avail (availNames)
import GHC.Types.TyThing (TyThing(..), implicitTyThings)
import GHC.Data.Maybe (MaybeErr(..))
import Numeric (showHex)

import Tidepool.ExecutionSource (WorkerExecutionSource, encodeWorkerExecutionSource)
import Tidepool.DependencyEvidence
  ( DependencyEvidence(..), DependencyModule(..), DependencySource(..)
  , ProductAvailability(..) )
import Tidepool.ExecutionSchema
  ( ConstructorDecl(..), ConstructorId(..), GlobalDecl(..), Group(..)
  , HeapBinding(..), HeapRhs(..), TopBinding(..), ProjectedGroup(..), ProjectedGroupBody(..)
  , ResultContract(..), RuntimeRep(..), Signature(..), SignatureId(..)
  , SymbolIdentity(..), WireProgram(..) )
import Tidepool.ExactScope
  ( ExactScope(..), scopeInterfaces, ExactProduct(..), ExactOriginalGroup(..), scopeValueInterfaces
  , CanonicalInterfaceProof, canonicalCertificateSha256, canonicalSourceSha256 )
import Tidepool.ModuleCandidates
  ( CandidateGlobal(..), CandidateGroup(..), ModuleCandidate(..) )
import Tidepool.PackageWitness
  ( PackageImportRoot(..), packageImportRoot, validatePackageImportRoot )
import Tidepool.FatIface (readExactInterface)
import Tidepool.ExactHydration (ExactIfaceArtifact(..))
import Tidepool.FinalizedModule (homeInterfaceUsageOwners)
import Tidepool.FinalizedModuleArtifacts
  ( FinalizedModuleArtifacts, encodeFinalizedModuleArtifacts, finalizedInterfaceSeals )
import Tidepool.Timing (readTimingEnabled, emitCount)

data ProductOrigin = FreshProduct | CachedProduct | RetainedCoreProduct deriving (Eq)

renderProductOrigin :: ProductOrigin -> T.Text
renderProductOrigin FreshProduct = "fresh"
renderProductOrigin CachedProduct = "cached"
renderProductOrigin RetainedCoreProduct = "retained-core"

data Product = Product
  { productOrigin :: ProductOrigin
  , productUnit :: T.Text
  , productModule :: T.Text
  , productVersion :: Maybe T.Text
  , productSourceSha :: T.Text
  , productIfaceSha :: T.Text
  , productBytesSha :: T.Text
  , productEvidenceSha :: T.Text
  , productInterfaces :: [(T.Text, T.Text, T.Text)]
  , productGroups :: [CandidateGroup]
  }

type BinderOwner = (T.Text, T.Text, Maybe T.Text, Word)
type PackageWitness = (T.Text, T.Text, FilePath, T.Text)

-- An owner has the witnessed binder already carried by its global. Coordinates
-- remain independent: a source group's owner need not be the binder's module.
data OwnerCoordinate
  = SourceCoordinate T.Text T.Text (Maybe T.Text)
  | PackageCoordinate T.Text T.Text T.Text
  deriving (Eq, Ord)

data WitnessOwner
  = SourceOwner T.Text T.Text (Maybe T.Text) Word
  | RetainedOwner Word64
  | PackageOwner T.Text T.Text T.Text (Maybe Word64)
  deriving (Eq, Ord)

type GlobalWitness = (SymbolIdentity, RuntimeRep, Maybe Signature, Bool, WitnessOwner)

ownerCoordinate :: WitnessOwner -> Maybe OwnerCoordinate
ownerCoordinate (SourceOwner unit name version _) = Just (SourceCoordinate unit name version)
ownerCoordinate (PackageOwner unit name sha _) = Just (PackageCoordinate unit name sha)
ownerCoordinate (RetainedOwner _) = Nothing

encodeCoordinate :: OwnerCoordinate -> Encoding
encodeCoordinate (SourceCoordinate unit name version) = array
  [encodeString "source", encodeString unit, encodeString name
  , maybe encodeNull encodeString version]
encodeCoordinate (PackageCoordinate unit name sha) = array
  [encodeString "package", encodeString unit, encodeString name, encodeString sha]

encodeWitness :: Map.Map OwnerCoordinate Word -> GlobalWitness -> Encoding
encodeWitness coordinates (identity, rep, signature, evaluated, owner) = array
  [encodeIdentity identity, encodeRep rep, maybe encodeNull encodeSignature signature
  , encodeBool evaluated, encodeCompactOwner coordinates owner]

encodeCompactOwner :: Map.Map OwnerCoordinate Word -> WitnessOwner -> Encoding
encodeCompactOwner coordinates owner = case owner of
  SourceOwner unit name version ordinal -> array
    [encodeString "source", index (SourceCoordinate unit name version), encodeWord ordinal]
  RetainedOwner generation -> array [encodeString "retained", encodeWord64 generation]
  PackageOwner unit name sha Nothing -> array
    [encodeString "package", index (PackageCoordinate unit name sha)]
  PackageOwner unit name sha (Just generation) -> array
    [encodeString "retained-package", index (PackageCoordinate unit name sha), encodeWord64 generation]
  where index coordinate = encodeWord (coordinates Map.! coordinate)

-- The producer's group inventory is only a suggestion. Rust compares every
-- emitted row with the original sidecar bytes before admitting any product.
encodeCertifiedProducts
  :: HscEnv -> WorkerExecutionSource -> Map.Map ModuleName ModIface -> FinalizedModuleArtifacts -> [ModuleCandidate] -> Maybe ExactScope
  -> [(T.Text, T.Text, BS.ByteString, [ProjectedGroup])]
  -> [(String, WireProgram)]
  -> DependencyEvidence -> BS.ByteString -> BS.ByteString
  -> IO (Either String BS.ByteString)
encodeCertifiedProducts env sourceRecipe interfaces finalized cached exact fresh targets evidence productBytes evidenceBytes =
  fmap (fmap fst) (encodeCertifiedProductsWithOriginals Map.empty Map.empty env sourceRecipe interfaces finalized cached exact fresh targets evidence productBytes evidenceBytes)

encodeCertifiedProductsWithOriginals
  :: Map.Map (String,String) CanonicalInterfaceProof
  -> Map.Map (T.Text,T.Text) (T.Text,T.Text,T.Text,T.Text)
  -> HscEnv -> WorkerExecutionSource -> Map.Map ModuleName ModIface -> FinalizedModuleArtifacts -> [ModuleCandidate] -> Maybe ExactScope
  -> [(T.Text, T.Text, BS.ByteString, [ProjectedGroup])]
  -> [(String, WireProgram)]
  -> DependencyEvidence -> BS.ByteString -> BS.ByteString
  -> IO (Either String (BS.ByteString, Map.Map (String,String) String))
encodeCertifiedProductsWithOriginals retained emittedSeals env sourceRecipe interfaces finalized cached exact fresh targets evidence productBytes evidenceBytes = do
  packageRef <- newIORef []
  timing <- readTimingEnabled
  (resolvePackage, resolutionCounts) <- newPackageGlobalResolver timing env
  let freshEvidenceSha = digest evidenceBytes
      freshProductSha = digest productBytes
      freshProducts = catMaybes
        [ do
            let original = Map.lookup (T.unpack unit,T.unpack name) retained
            sourceSha <- case original of
              Nothing -> sourceProductSha256 evidence (T.unpack unit) (T.unpack name)
              Just proof -> Just (T.pack (canonicalSourceSha256 proof))
            groups <- traverse freshGroup projected
            pure Product
              { productOrigin = maybe FreshProduct (const RetainedCoreProduct) original, productUnit = unit, productModule = name
              , productVersion = Nothing, productSourceSha = sourceSha
              , productIfaceSha = digest iface
              , productBytesSha = freshProductSha
              , productEvidenceSha = maybe freshEvidenceSha (T.pack . canonicalCertificateSha256) original
              , productInterfaces = []
              , productGroups = groups
              }
        | (unit, name, iface, projected) <- fresh ]
      cachedProducts =
        [ Product
          { productOrigin = CachedProduct
          , productUnit = T.pack (candidateUnit candidate)
          , productModule = T.pack (candidateModule candidate)
          , productVersion = Just (T.pack (candidateModuleVersion candidate))
          , productSourceSha = T.pack (candidateSourceSha256 candidate)
          , productIfaceSha = T.pack (candidateInterfaceSha256 candidate)
          , productBytesSha = T.pack (candidateProductSha256 candidate)
          , productEvidenceSha = T.pack (candidateEvidenceSha256 candidate)
          , productInterfaces = []
          , productGroups = candidateGroups candidate
          } | candidate <- cached ]
      unsealedProducts = freshProducts ++ cachedProducts
      interfaceSeals = Map.fromListWith Set.union
        ([(owner,Set.singleton sha) | (owner,sha) <- finalizedInterfaceSeals finalized]
        ++ [( (productUnit product, productModule product), Set.singleton (productIfaceSha product))
          | product <- unsealedProducts]
        ++ [((T.pack (exactUnit iface), T.pack (exactModule iface)), Set.singleton (T.pack (exactSha256 iface)))
           | scope <- maybe [] pure exact, (iface, _, _) <- scopeInterfaces scope]
        ++ [((T.pack (exactUnit iface), T.pack (exactModule iface)), Set.singleton (T.pack (exactSha256 iface)))
           | scope <- maybe [] pure exact, iface <- scopeValueInterfaces scope])
  sealedProducts <- forM unsealedProducts $ \product -> do
    let owner = mkModule (stringToUnit (T.unpack (productUnit product))) (mkModuleName (T.unpack (productModule product)))
    selected <- case Map.lookup (moduleName owner) interfaces of
      Just iface | mi_module iface == owner -> pure (Right iface)
      _ -> fmap (either (const (Left "original interface usage inventory unavailable")) (Right . fst))
        (readExactInterface env owner)
    pure $ do
      iface <- selected
      requirements <- forM (homeInterfaceUsageOwners env iface) $ \(unit, name) ->
        case Map.lookup (T.pack unit, T.pack name) interfaceSeals of
          Just seals | [sha] <- Set.toAscList seals -> Right (T.pack unit, T.pack name, sha)
          selectedSeals -> Left ("original interface usage leaves the exact sealed owner closure: "
            ++ show (productUnit product, productModule product)
            ++ " requires " ++ show (unit,name)
            ++ "; selected seals=" ++ show (maybe [] Set.toAscList selectedSeals))
      Right product { productInterfaces = requirements }
  let products = [product | Right product <- sealedProducts]
      expectedFresh = length fresh
      admittedBinders =
        [ (binder, (T.pack (originalUnit product), T.pack (originalModule product),
                    Just (T.pack (originalVersion product)), originalOrdinal group))
        | scope <- maybe [] pure exact, product <- scopeProducts scope
        , group <- originalGroups product, binder <- originalBinders group ]
      binders = admittedBinders ++
        [ (binder, (productUnit product, productModule product,
                    productVersion product, candidateGroupOrdinal group))
        | product <- products, group <- productGroups product
        , binder <- candidateGroupBinders group ]
      ownerMap = Map.fromList binders
      homeModules = Set.fromList (
        [ (T.pack (dependencyModuleUnit node), T.pack (dependencyModuleName node))
        | node <- dependencyModules evidence, not (dependencyModuleBoot node) ]
        ++ [(T.pack (exactUnit iface), T.pack (exactModule iface))
           | scope <- maybe [] pure exact, (iface, _, _) <- scopeInterfaces scope]
        ++ [(T.pack (exactUnit iface), T.pack (exactModule iface))
           | scope <- maybe [] pure exact, iface <- scopeValueInterfaces scope])
      allGlobals =
        [ (product, group) | product <- products, group <- productGroups product ]
  if any ((/= 1) . Set.size) (Map.elems interfaceSeals)
      || length products /= length unsealedProducts
      || length freshProducts /= expectedFresh
      || length binders /= Map.size ownerMap
    then pure (Left (case [reason | Left reason <- sealedProducts] of
      reason : _ -> reason
      [] | any ((/= 1) . Set.size) (Map.elems interfaceSeals) -> "conflicting original interface owner seals"
         | otherwise -> "incomplete fresh source or duplicate original binder inventory"))
    else do
      modules <- forM allGlobals $ \(product, group) -> do
        globals <- forM (candidateGroupGlobals group) $ \global ->
          encodeGlobalWitness env resolvePackage packageRef ownerMap homeModules
            (candidateGlobalIdentity global)
            (candidateGlobalRep global)
            (candidateGlobalSignature global)
            (candidateGlobalEvaluated global)
            (fromIntegral <$> candidateGlobalGeneration global)
        pure (product, group, sequence globals)
      targetRows <- forM targets $ \(target, program) -> do
        globals <- forM (programGlobals program) $ \global ->
          encodeGlobalWitness env resolvePackage packageRef ownerMap homeModules
            (globalIdentity global) (globalRep global)
            (globalEntrySignature global >>= \(SignatureId index) ->
              at (programSignatures program) (fromIntegral index))
            (globalRequiredEvaluated global) (globalRequiredGeneration global)
        pure (target, sequence globals)
      certifyLocalPackageExports env resolvePackage packageRef (map snd targets)
      packageWitnesses <- readIORef packageRef
      let packages = Map.fromListWith Set.union
            [ ((unit, name), Set.singleton (path, sha))
            | (unit, name, path, sha) <- packageWitnesses ]
      (requests, loads, catalogs) <- resolutionCounts
      let references = Map.fromListWith (+)
            [((path, sha), 1) | (_, _, path, sha) <- packageWitnesses]
      validated <- forM (Map.toAscList references) $ \((path, sha), count) -> do
        readBack <- try (BS.readFile path) :: IO (Either IOException BS.ByteString)
        pure $ case readBack of
          Left _ -> (False, 0, 0)
          Right bytes -> let size = fromIntegral (BS.length bytes)
            in (digest bytes == sha, size, size * count)
      let packagesValid = and [valid | (valid, _, _) <- validated]
      emitCount timing "certified_package_global_requests" requests
      emitCount timing "certified_package_owner_loads" loads
      emitCount timing "certified_package_catalog_builds" catalogs
      emitCount timing "certified_package_owner_revalidations" (fromIntegral (Map.size references))
      emitCount timing "certified_package_revalidation_bytes" (sum [size | (_, size, _) <- validated])
      emitCount timing "certified_package_reference_bytes" (sum [size | (_, _, size) <- validated])
      let failures = [errorText | (_, _, Left errorText) <- modules]
            ++ [errorText | (_, Left errorText) <- targetRows]
            ++ ["package interface changed during certification" | not packagesValid]
            ++ ["package owner selected more than one interface" |
                any ((/= 1) . Set.size) (Map.elems packages)]
      case failures of
        first : _ -> pure (Left first)
        [] -> do
          let moduleRows =
                [(product, group, witnesses)
                | (product, group, Right witnesses) <- modules]
              targetRowsReady =
                [(target, witnesses) | (target, Right witnesses) <- targetRows]
              allWitnessRows = map third moduleRows ++ map snd targetRowsReady
              (witnessIndices, referenceRows) = mapAccumL
                (mapAccumL internWitness) Map.empty allWitnessRows
              orderedWitnesses = Map.toAscList witnessIndices
              globalWitnesses = map fst orderedWitnesses
              coordinates = Set.toAscList (Set.fromList
                [coordinate | (_, _, _, _, owner) <- globalWitnesses
                , Just coordinate <- [ownerCoordinate owner]])
              coordinateIndices = Map.fromList (zip coordinates [0 ..])
              canonicalIndices = Map.fromList
                [(provisional, fromIntegral index)
                | (index, (_, provisional)) <- zip [0 :: Int ..] orderedWitnesses]
              encodeReference provisional = encodeWord
                (canonicalIndices Map.! provisional)
              (moduleReferenceRows, targetReferenceRows) =
                splitAt (length moduleRows) referenceRows
              byModule = Map.map reverse $ Map.fromListWith (++)
                [ ((productUnit product, productModule product),
                   [(candidateGroupOrdinal group, map encodeReference referenceIndices)])
                | ((product, group, _), referenceIndices) <- zip moduleRows moduleReferenceRows ]
              encodedModules =
                [ encodeModule product (Map.findWithDefault []
                    (productUnit product, productModule product) byModule)
                | product <- products ]
              encodedTargets =
                [ array [encodeString (T.pack target), list encodeReference referenceIndices]
                | ((target, _), referenceIndices) <- zip targetRowsReady targetReferenceRows ]
              encodedPackages =
                [ array [encodeString unit, encodeString name
                  , encodeString (T.pack path), encodeString sha]
                | ((unit, name), options) <- Map.toList packages
                , (path, sha) <- Set.toList options ]
              certificateBytes = toStrictByteString $ array
                [encodeString "TPCERT", encodeWord 9
                , list id encodedModules, list id encodedTargets
                , list id encodedPackages, list (encodeWitness coordinateIndices) globalWitnesses
                , encodeFinalizedModuleArtifacts finalized, encodeWorkerExecutionSource sourceRecipe
                , list encodeCoordinate coordinates]
          pure $ do
            versions <- retainedNativeVersions emittedSeals exact cached packages products moduleRows
            Right (certificateBytes,versions)
  where
    internWitness :: Map.Map GlobalWitness Word -> GlobalWitness
      -> (Map.Map GlobalWitness Word, Word)
    internWitness indices witness = case Map.lookup witness indices of
      Just index -> (indices, index)
      Nothing ->
        let index = fromIntegral (Map.size indices)
        in (Map.insert witness index indices, index)
    third (_, _, value) = value

-- Native identity binds the finite graph actually retained for this owner.
-- Promoted edges name nodes rather than recursively derived versions, so cycles
-- remain finite and unrelated request scope cannot change native authority.
retainedNativeVersions
  :: Map.Map (T.Text,T.Text) (T.Text,T.Text,T.Text,T.Text)
  -> Maybe ExactScope -> [ModuleCandidate]
  -> Map.Map (T.Text,T.Text) (Set.Set (FilePath,T.Text))
  -> [Product] -> [(Product,CandidateGroup,[GlobalWitness])]
  -> Either String (Map.Map (String,String) String)
retainedNativeVersions emitted exact cached packages products rows = do
  versions <- forM (Map.keys promoted) $ \root -> do
    reachable <- closure Set.empty [root]
    nodes <- traverse encodeNode (Set.toAscList reachable)
    let graph = toStrictByteString (list id nodes)
        (unit,name) = root
        framed bytes = BS.pack [fromIntegral ((fromIntegral (BS.length bytes) :: Word64) `shiftR` shift)
          | shift <- [56,48..0]] <> bytes
        version = digest (BS.concat (map framed
          ["retained-core-home-v1",TE.encodeUtf8 unit,TE.encodeUtf8 name,graph]))
    pure ((T.unpack unit,T.unpack name),T.unpack version)
  pure (Map.fromList versions)
  where
    key product = (productUnit product,productModule product)
    promoted = Map.fromList [(key product,product) | product <- products
      , productOrigin product == RetainedCoreProduct]
    groups = Map.union (Map.fromListWith Map.union
      [(key product,Map.singleton (candidateGroupOrdinal group) witnesses)
      | (product,group,witnesses) <- rows]) (Map.map (const Map.empty) promoted)
    seals = Map.unions [emitted,Map.fromList
      [((T.pack (candidateUnit candidate),T.pack (candidateModule candidate)),
        (T.pack (candidateModuleVersion candidate),T.pack (candidateInterfaceSha256 candidate),
         T.pack (candidateProductSha256 candidate),T.pack (candidatePackageImportsSha256 candidate)))
      | candidate <- cached],Map.fromList
      [((T.pack (originalUnit product),T.pack (originalModule product)),
        (T.pack (originalVersion product),T.pack (originalIfaceSha256 product),
         T.pack (originalProductSha256 product),""))
      | scope <- maybe [] pure exact,product <- scopeProducts scope]]
    lookupSeal owner = maybe (Left "native demand graph lacks an actual source owner seal") Right (Map.lookup owner seals)
    closure visited [] = Right visited
    closure visited (owner:pending)
      | owner `Set.member` visited = closure visited pending
      | otherwise = case Map.lookup owner groups of
          Nothing -> Left "native demand graph lacks a promoted group inventory"
          Just owned -> closure (Set.insert owner visited)
            ([dependency | witnesses <- Map.elems owned,(_,_,_,_,SourceOwner unit name _ _) <- witnesses
              , let dependency = (unit,name),Map.member dependency promoted] ++ pending)
    encodeNode owner@(unit,name) = do
      product <- maybe (Left "native demand graph lacks a promoted canonical proof") Right (Map.lookup owner promoted)
      (_,_,nativeSha,packageSha) <- lookupSeal owner
      owned <- maybe (Left "native demand graph lacks retained groups") Right (Map.lookup owner groups)
      encodedGroups <- forM (Map.toAscList owned) $ \(ordinal,witnesses) -> do
        edges <- traverse encodeEdge witnesses
        pure (array [encodeWord ordinal,list id edges])
      pure (array [encodeString unit,encodeString name,encodeString (productEvidenceSha product),
        encodeString nativeSha,encodeString packageSha,list id encodedGroups])
    encodeEdge (identity,_,_,_,owner) = case owner of
      SourceOwner unit name _ ordinal
        | Map.member (unit,name) promoted -> Right (array
            [encodeString "local-source",encodeString unit,encodeString name,encodeWord ordinal,encodeIdentity identity])
        | otherwise -> do
            (version,ifaceSha,nativeSha,_) <- lookupSeal (unit,name)
            pure (array [encodeString "source",array (map encodeString [unit,name,version,ifaceSha,nativeSha]),
              encodeWord ordinal,encodeIdentity identity])
      RetainedOwner generation -> Right (array
        [encodeString "retained",encodeIdentity identity,encodeWord64 generation])
      PackageOwner unit name sha generation -> do
        path <- case Map.lookup (unit,name) packages of
          Just options | [(selected,selectedSha)] <- Set.toAscList options,selectedSha == sha -> Right selected
          _ -> Left "native demand graph lacks an exact package path seal"
        pure (array ([encodeString (maybe "package" (const "retained-package") generation),
          encodeString unit,encodeString name,encodeString sha,encodeIdentity identity]
          ++ maybe [] (pure . encodeWord64) generation ++ [encodeString (T.pack path)]))

-- A locally emitted package value may have no incoming global edge. Its exact
-- defining interface must still be retained before it is offered to later
-- turns. One canonical binder proves each module's selected interface; later
-- retained-package demands independently prove their own canonical binder.
-- Noncanonical compiler helpers remain internal and do not supply evidence.
certifyLocalPackageExports :: HscEnv -> PackageGlobalResolver
  -> IORef [PackageWitness] -> [WireProgram] -> IO ()
certifyLocalPackageExports env resolvePackage packageRef programs = do
  observed <- readIORef packageRef
  let witnessed = Set.fromList [(unit, name) | (unit, name, _, _) <- observed]
      candidates = Map.fromListWith (++)
        [ ((symbolUnit identity, symbolModule identity), [identity])
        | program <- programs, group <- programBindings program
        , TopBinding identity binding <- case group of
            NonRecursive top -> [top]
            Recursive tops -> tops
        , not (toUnitId (moduleUnit (symbolOwner identity)) `Set.member` hsc_all_home_unit_ids env)
        , symbolNamespace identity == "value"
        , eligible program (heapBindingRhs binding) ]
  mapM_ (select . Set.toAscList . Set.fromList . snd)
    (Map.toAscList (Map.withoutKeys candidates witnessed))
  where
    eligible _ (Bytes _) = False
    eligible program (Constructor (ConstructorId index) _) =
      maybe False ((== LiftedRefRep) . constructorResultRep)
        (at (programConstructors program) (fromIntegral index))
    eligible _ _ = True
    select [] = pure ()
    select (identity : rest) = resolvePackage identity >>= \case
      Left _ -> select rest
      Right (_, witness) -> modifyIORef' packageRef
        ((symbolUnit identity, symbolModule identity, packagePath witness,
          T.pack (packageSha256 witness)) :)

freshGroup :: ProjectedGroup -> Maybe CandidateGroup
freshGroup group = do
  let body = projectedBody group
  globals <- traverse (\global -> do
    signature <- case globalEntrySignature global of
      Nothing -> Just Nothing
      Just (SignatureId index) -> Just <$> at
        (projectedSignatures body) (fromIntegral index)
    pure CandidateGlobal
      { candidateGlobalIdentity = globalIdentity global
      , candidateGlobalRep = globalRep global
      , candidateGlobalSignature = signature
      , candidateGlobalEvaluated = globalRequiredEvaluated global
      , candidateGlobalGeneration = fromIntegral <$>
          globalRequiredGeneration global
      }) (projectedGlobals body)
  pure (CandidateGroup (fromIntegral (projectedOriginalOrdinal group))
    (projectedBinders group) globals)

at :: [a] -> Int -> Maybe a
at _ index | index < 0 = Nothing
at values index = case drop index values of
  value : _ -> Just value
  [] -> Nothing

sourceProductSha256 :: DependencyEvidence -> String -> String -> Maybe T.Text
sourceProductSha256 evidence unit name = do
  node <- find (\item -> dependencyModuleUnit item == unit
    && dependencyModuleName item == name
    && not (dependencyModuleBoot item)
    && dependencyModuleProduct item == ProductReady)
    (dependencyModules evidence)
  source <- find ((== dependencyModuleSource node) . dependencySourcePath)
    (dependencySources evidence)
  pure (T.pack (dependencySourceSha256 source))

encodeGlobalWitness
  :: HscEnv -> PackageGlobalResolver -> IORef [PackageWitness] -> Map.Map SymbolIdentity BinderOwner
  -> Set.Set (T.Text, T.Text)
  -> SymbolIdentity -> RuntimeRep -> Maybe Signature -> Bool -> Maybe Word64
  -> IO (Either String GlobalWitness)
encodeGlobalWitness env resolvePackage packageRef binders homeModules identity rep signature evaluated generation = do
  selected <- case generation of
    Just wanted
      | toUnitId (moduleUnit (symbolOwner identity)) `Set.member` hsc_all_home_unit_ids env ->
          pure (Right (RetainedOwner wanted))
      | otherwise -> packageOwner resolvePackage packageRef identity (Just wanted)
    Nothing -> case Map.lookup identity binders of
      Just (unit, name, version, ordinal) -> pure (Right (SourceOwner unit name version ordinal))
      Nothing
        | (symbolUnit identity, symbolModule identity) `Set.member` homeModules
            || toUnitId (moduleUnit (symbolOwner identity)) `Set.member` hsc_all_home_unit_ids env ->
            pure (Left ("external home global has no certified source group: " ++ show identity))
        | otherwise -> packageOwner resolvePackage packageRef identity Nothing
  pure $ do
    owner <- selected
    Right (identity, rep, signature, evaluated, owner)

symbolOwner :: SymbolIdentity -> Module
symbolOwner identity = mkModule (stringToUnit (T.unpack (symbolUnit identity)))
  (mkModuleName (T.unpack (symbolModule identity)))

packageOwner :: PackageGlobalResolver -> IORef [PackageWitness] -> SymbolIdentity -> Maybe Word64
  -> IO (Either String WitnessOwner)
packageOwner resolvePackage packageRef identity generation = do
  selected <- resolvePackage identity
  case selected of
    Left reason -> pure (Left reason)
    Right (_, witness) -> do
      let sha = T.pack (packageSha256 witness)
      modifyIORef' packageRef ((symbolUnit identity,
        symbolModule identity, packagePath witness, sha) :)
      pure (Right (PackageOwner
        (symbolUnit identity) (symbolModule identity) sha generation))

type PackageGlobalResolver = SymbolIdentity -> IO (Either String (Id, PackageImportRoot))

data PackageDeclarationCatalog = PackageDeclarationCatalog
  { catalogOwner :: Module
  , catalogParents :: Map.Map OccName [Name]
  , catalogWiredGlobals :: Map.Map OccName (Map.Map Name TyThing)
  }

-- The captured interface owns canonical parent Names and wired exports. Index
-- their occurrences once; loading a demanded parent still uses GHC's interface
-- loader and never reconstructs a Name from its spelling.
packageDeclarationCatalog :: Module -> ModIface -> PackageDeclarationCatalog
packageDeclarationCatalog owner iface = PackageDeclarationCatalog owner parents wired
  where
    owned name = nameModule_maybe name == Just owner
    parents = Map.fromListWith (flip (++))
      [ (occurrence, [ifName declaration])
      | (_, declaration) <- mi_decls iface
      , owned (ifName declaration)
      , occurrence <- Set.toAscList (Set.fromList
          (nameOccName (ifName declaration) : ifaceDeclImplicitBndrs declaration))
      , isVarOcc occurrence ]
    wired = Map.fromListWith Map.union
      [ (nameOccName name, Map.singleton name thing)
      | (name, thing) <- packageWiredGlobals owner iface ]

packageWiredGlobals :: Module -> ModIface -> [(Name, TyThing)]
packageWiredGlobals owner iface =
  [ (name, thing)
  | available <- mi_exports iface, exported <- availNames available
  , owned exported
  , parent <- maybe [] pure (wiredInNameTyThing_maybe exported)
  , thing@(AnId _) <- parent : implicitTyThings parent
  , let name = getName thing
  , owned name, isVarOcc (nameOccName name) ]
  where
    owned name = nameModule_maybe name == Just owner

type PackageGlobalLookup = OccName -> IO (MaybeErr () TyThing)

-- This capture belongs to one certification. A module's defining interface is
-- checked around its read; each demanded symbol is still resolved separately.
-- Successful witnesses are checked again before publication, and nothing is
-- retained across requests or changes to the compiler environment.
newPackageGlobalResolver :: Bool -> HscEnv -> IO (PackageGlobalResolver, IO (Integer, Integer, Integer))
newPackageGlobalResolver timing env = do
  state <- newIORef (0, Map.empty)
  let load owner = do
        (_, interfaces) <- readIORef state
        case Map.lookup owner interfaces of
          Just value -> pure value
          Nothing -> do
            loaded <- loadPackageInterface env owner
            value <- case loaded of
              Left reason -> pure (Left reason)
              Right (iface, witness) -> do
                unchanged <- validatePackageImportRoot env witness
                let catalog = packageDeclarationCatalog owner iface
                pure ((canonicalPackageGlobal env catalog, witness) <$ unchanged)
            modifyIORef' state (\(requests, selected) ->
              (requests, Map.insert owner value selected))
            pure value
      resolve identity = do
        when timing $ modifyIORef' state (\(requests, interfaces) ->
          let next = requests + 1 in next `seq` (next, interfaces))
        resolvePackageGlobalUsing load identity
      counts = do
        (requests, interfaces) <- readIORef state
        pure (requests, fromIntegral (Map.size interfaces),
          fromIntegral (length [() | Right _ <- Map.elems interfaces]))
  pure (resolve, counts)

-- Recovery and certification share one canonical package owner. The Id comes
-- from the exact interface's structural declaration, including implicit tops.
resolvePackageGlobal :: HscEnv -> PackageGlobalResolver
resolvePackageGlobal env identity = do
  let load owner = do
        found <- loadPackageInterface env owner
        pure $ fmap (\(iface, witness) ->
          (canonicalPackageGlobalOnce env owner iface, witness)) found
  selected <- resolvePackageGlobalUsing load identity
  case selected of
    Left reason -> pure (Left reason)
    Right (identifier, witness) -> do
      unchanged <- validatePackageImportRoot env witness
      pure $ case unchanged of
        Left reason -> Left (packageRefusal identity reason)
        Right () -> Right (identifier, witness)

loadPackageInterface :: HscEnv -> Module
  -> IO (Either String (ModIface, PackageImportRoot))
loadPackageInterface env owner = do
  found <- packageImportRoot env owner
  case found of
    Left reason -> pure (Left reason)
    Right witness -> do
      exact <- readExactInterface env owner
      case exact of
        Right (iface, location) | ml_hi_file location == packagePath witness ->
          pure (Right (iface, witness))
        _ -> pure (Left "selected package interface is unavailable or changed")

resolvePackageGlobalUsing :: (Module -> IO (Either String (PackageGlobalLookup, PackageImportRoot)))
  -> PackageGlobalResolver
resolvePackageGlobalUsing load identity
  | symbolNamespace identity /= "value" =
      pure (Left (refusal "unsupported external global namespace"))
  | otherwise = do
      let owner = symbolOwner identity
      found <- load owner
      case found of
        Left reason -> pure (Left (refusal reason))
        Right (lookupGlobal, witness) -> do
          selected <- lookupGlobal (mkVarOcc (T.unpack (symbolOccurrence identity)))
          pure $ case selected of
            Succeeded (AnId identifier) -> Right (identifier, witness)
            _ -> Left (refusal "selected package global is absent from loaded interface")
  where
    refusal = packageRefusal identity

packageRefusal :: SymbolIdentity -> String -> String
packageRefusal identity reason = reason ++ ": " ++ T.unpack (symbolUnit identity) ++ ":"
  ++ T.unpack (symbolModule identity) ++ "." ++ T.unpack (symbolOccurrence identity)

-- The interface supplies canonical Names, including known-key bindings.
-- An implicit Id is authenticated by its defining parent declaration; wired
-- parents are authenticated by the exact interface's exported canonical Name.
-- Reconstructing a Name from Module/OccName can mint a different Unique.
canonicalPackageGlobal :: HscEnv -> PackageDeclarationCatalog -> OccName
  -> IO (MaybeErr () TyThing)
canonicalPackageGlobal env catalog wanted = canonicalPackageCandidates env
  (catalogOwner catalog)
  (Map.findWithDefault [] wanted (catalogParents catalog))
  (Map.findWithDefault Map.empty wanted (catalogWiredGlobals catalog)) wanted

-- A standalone recovery lookup scans only its demanded occurrence. It has no
-- certification owner to amortize construction of the complete catalog.
canonicalPackageGlobalOnce :: HscEnv -> Module -> ModIface -> PackageGlobalLookup
canonicalPackageGlobalOnce env owner iface wanted = canonicalPackageCandidates env owner
  [ ifName declaration
  | (_, declaration) <- mi_decls iface
  , nameModule_maybe (ifName declaration) == Just owner
  , nameOccName (ifName declaration) == wanted
      || wanted `elem` ifaceDeclImplicitBndrs declaration ]
  (Map.fromList [(name, thing) | (name, thing) <- packageWiredGlobals owner iface
    , nameOccName name == wanted]) wanted

canonicalPackageCandidates :: HscEnv -> Module -> [Name] -> Map.Map Name TyThing
  -> PackageGlobalLookup
canonicalPackageCandidates env owner declarations wired wanted = do
  parents <- mapM loadCanonical declarations
  let owned name = nameModule_maybe name == Just owner
      declared = Map.fromList
        [ (name, thing)
        | Succeeded parent <- parents
        , thing@(AnId _) <- parent : implicitTyThings parent
        , let name = getName thing
        , owned name, isVarOcc (nameOccName name), nameOccName name == wanted ]
      candidates = Map.union wired declared
  pure $ case Map.elems candidates of
    [thing] -> Succeeded thing
    _ -> Failed ()
  where
    loadCanonical :: Name -> IO (MaybeErr () TyThing)
    loadCanonical name = case wiredInNameTyThing_maybe name of
      Just thing -> pure (Succeeded thing)
      Nothing -> do
        result <- initIfaceLoad env (importDecl name)
        pure $ case result of
          Failed _ -> Failed ()
          Succeeded thing -> Succeeded thing

encodeModule :: Product -> [(Word, [Encoding])] -> Encoding
encodeModule product groups = array
  [ encodeString (renderProductOrigin (productOrigin product))
  , encodeString (productUnit product), encodeString (productModule product)
  , maybe encodeNull encodeString (productVersion product)
  , encodeString (productSourceSha product)
  , encodeString (productIfaceSha product)
  , encodeString (productBytesSha product)
  , encodeString (productEvidenceSha product)
  , list (\(ordinal, globals) -> array [encodeWord ordinal, list id globals]) groups
  , list (\(unit, name, sha) -> array [encodeString unit, encodeString name, encodeString sha])
      (productInterfaces product)
  ]

encodeIdentity :: SymbolIdentity -> Encoding
encodeIdentity identity = array
  [ encodeString (symbolUnit identity), encodeString (symbolModule identity)
  , encodeString (symbolNamespace identity), encodeString (symbolOccurrence identity)
  , maybe encodeNull encodeString (symbolRecordParent identity) ]

encodeRep :: RuntimeRep -> Encoding
encodeRep rep = case rep of
  VoidRep -> named "void" 0
  LiftedRefRep -> named "lifted" 0
  UnliftedRefRep -> named "unlifted" 0
  AddressRep -> named "address" 0
  IntRep bits -> named "int" (fromIntegral bits)
  WordRep bits -> named "word" (fromIntegral bits)
  FloatRep bits -> named "float" (fromIntegral bits)
  where named name width = array [encodeString name, encodeWord width]

encodeSignature :: Signature -> Encoding
encodeSignature signature = array
  [ list encodeRep (signatureArguments signature)
  , case signatureResults signature of
      Returns reps -> array [encodeString "returns", list encodeRep reps]
      NoSuccess -> array [encodeString "no_success", list encodeRep []]
      CallerResult -> array [encodeString "caller_result", list encodeRep []]
  ]

array :: [Encoding] -> Encoding
array fields = encodeListLen (fromIntegral (length fields)) <> fold fields

list :: (a -> Encoding) -> [a] -> Encoding
list encode values = encodeListLen (fromIntegral (length values)) <> foldMap encode values

digest :: BS.ByteString -> T.Text
digest bytes = T.pack (concatMap byteHex (BS.unpack (SHA256.hash bytes)))
  where byteHex byte = let rendered = showHex byte "" in
          replicate (2 - length rendered) '0' ++ rendered
