{-# LANGUAGE OverloadedStrings #-}

module Tidepool.CertifiedProducts
  ( encodeCertifiedProducts, resolvePackageGlobal ) where

import Prelude hiding (product)
import Codec.CBOR.Encoding
  ( Encoding, encodeBool, encodeListLen, encodeNull, encodeString, encodeWord
  , encodeWord64 )
import Codec.CBOR.Write (toStrictByteString)
import Control.Exception (IOException, try)
import Control.Monad (forM)
import qualified Crypto.Hash.SHA256 as SHA256
import qualified Data.ByteString as BS
import Data.Foldable (fold)
import Data.List (find)
import qualified Data.Map.Strict as Map
import Data.Maybe (catMaybes, mapMaybe)
import qualified Data.Set as Set
import Data.IORef (IORef, newIORef, modifyIORef', readIORef)
import qualified Data.Text as T
import Data.Word (Word64)
import GHC.Driver.Env (HscEnv)
import GHC.Unit.Module (Module, mkModule, mkModuleName)
import GHC.Unit.Module.ModIface (ModIface, mi_decls, mi_exports)
import GHC.Unit.Module.Location (ml_hi_file)
import GHC.Unit.Types (stringToUnit)
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

import Tidepool.DependencyEvidence
  ( DependencyEvidence(..), DependencyModule(..), DependencySource(..)
  , ProductAvailability(..) )
import Tidepool.ExecutionSchema
  ( GlobalDecl(..), ProjectedGroup(..), ProjectedGroupBody(..)
  , ResultContract(..), RuntimeRep(..), Signature(..), SignatureId(..)
  , SymbolIdentity(..), WireProgram(..) )
import Tidepool.ExactScope
  ( ExactScope(..), ExactProduct(..), ExactOriginalGroup(..) )
import Tidepool.ModuleCandidates
  ( CandidateGlobal(..), CandidateGroup(..), ModuleCandidate(..) )
import Tidepool.PackageWitness
  ( PackageImportRoot(..), packageImportRoot, validatePackageImportRoot )
import Tidepool.FatIface (readExactInterface)
import Tidepool.ExactHydration (ExactIfaceArtifact(..))

data Product = Product
  { productOrigin :: T.Text
  , productUnit :: T.Text
  , productModule :: T.Text
  , productVersion :: Maybe T.Text
  , productSourceSha :: T.Text
  , productIfaceSha :: T.Text
  , productBytesSha :: T.Text
  , productEvidenceSha :: T.Text
  , productGroups :: [CandidateGroup]
  }

type BinderOwner = (T.Text, T.Text, Maybe T.Text, Word)
type PackageWitness = (T.Text, T.Text, FilePath, T.Text)

-- The producer's group inventory is only a suggestion. Rust compares every
-- emitted row with the original sidecar bytes before admitting any product.
encodeCertifiedProducts
  :: HscEnv -> [ModuleCandidate] -> Maybe ExactScope
  -> [(T.Text, T.Text, BS.ByteString, [ProjectedGroup])]
  -> [(String, WireProgram)]
  -> DependencyEvidence -> BS.ByteString -> BS.ByteString
  -> IO (Either String BS.ByteString)
encodeCertifiedProducts env cached exact fresh targets evidence productBytes evidenceBytes = do
  packageRef <- newIORef []
  let freshEvidenceSha = digest evidenceBytes
      freshProductSha = digest productBytes
      freshProducts = catMaybes
        [ do
            sourceSha <- sourceHash evidence (T.unpack unit) (T.unpack name)
            groups <- traverse freshGroup projected
            pure Product
              { productOrigin = "fresh", productUnit = unit, productModule = name
              , productVersion = Nothing, productSourceSha = sourceSha
              , productIfaceSha = digest iface
              , productBytesSha = freshProductSha
              , productEvidenceSha = freshEvidenceSha
              , productGroups = groups
              }
        | (unit, name, iface, projected) <- fresh ]
      cachedProducts =
        [ Product
          { productOrigin = "cached"
          , productUnit = T.pack (candidateUnit candidate)
          , productModule = T.pack (candidateModule candidate)
          , productVersion = Just (T.pack (candidateModuleVersion candidate))
          , productSourceSha = T.pack (candidateSourceSha256 candidate)
          , productIfaceSha = T.pack (candidateInterfaceSha256 candidate)
          , productBytesSha = T.pack (candidateProductSha256 candidate)
          , productEvidenceSha = T.pack (candidateEvidenceSha256 candidate)
          , productGroups = candidateGroups candidate
          } | candidate <- cached ]
      products = freshProducts ++ cachedProducts
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
           | scope <- maybe [] pure exact, (iface, _, _) <- scopeInterfaces scope])
      allGlobals =
        [ (product, group) | product <- products, group <- productGroups product ]
  if length freshProducts /= expectedFresh
      || length binders /= Map.size ownerMap
    then pure (Left "incomplete fresh source or duplicate original binder inventory")
    else do
      modules <- forM allGlobals $ \(product, group) -> do
        globals <- forM (candidateGroupGlobals group) $ \global ->
          encodeGlobalWitness env packageRef ownerMap homeModules
            (candidateGlobalIdentity global)
            (candidateGlobalRep global)
            (candidateGlobalSignature global)
            (candidateGlobalEvaluated global)
            (fromIntegral <$> candidateGlobalGeneration global)
        pure (product, group, sequence globals)
      targetRows <- forM targets $ \(target, program) -> do
        globals <- forM (programGlobals program) $ \global ->
          encodeGlobalWitness env packageRef ownerMap homeModules
            (globalIdentity global) (globalRep global)
            (globalEntrySignature global >>= \(SignatureId index) ->
              at (programSignatures program) (fromIntegral index))
            (globalRequiredEvaluated global) (globalRequiredGeneration global)
        pure (target, sequence globals)
      packageWitnesses <- readIORef packageRef
      let packages = Map.fromListWith Set.union
            [ ((unit, name), Set.singleton (path, sha))
            | (unit, name, path, sha) <- packageWitnesses ]
      packagesValid <- and <$> forM packageWitnesses (\(_, _, path, sha) -> do
        readBack <- try (BS.readFile path) :: IO (Either IOException BS.ByteString)
        pure (either (const False) ((== sha) . digest) readBack))
      let failures = [errorText | (_, _, Left errorText) <- modules]
            ++ [errorText | (_, Left errorText) <- targetRows]
            ++ ["package interface changed during certification" | not packagesValid]
            ++ ["package owner selected more than one interface" |
                any ((/= 1) . Set.size) (Map.elems packages)]
      case failures of
        first : _ -> pure (Left first)
        [] -> do
          let byModule = Map.fromListWith (flip (++))
                [ ((productUnit product, productModule product),
                   [(candidateGroupOrdinal group, witnesses)])
                | (product, group, Right witnesses) <- modules ]
              encodedModules =
                [ encodeModule product (Map.findWithDefault []
                    (productUnit product, productModule product) byModule)
                | product <- products ]
              encodedTargets =
                [ array [encodeString (T.pack target), list id witnesses]
                | (target, Right witnesses) <- targetRows ]
              encodedPackages =
                [ array [encodeString unit, encodeString name
                  , encodeString (T.pack path), encodeString sha]
                | ((unit, name), options) <- Map.toList packages
                , (path, sha) <- Set.toList options ]
          pure (Right (toStrictByteString (array
            [encodeString "TPCERT", encodeWord 2
            , list id encodedModules, list id encodedTargets
            , list id encodedPackages])))

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

sourceHash :: DependencyEvidence -> String -> String -> Maybe T.Text
sourceHash evidence unit name = do
  node <- find (\item -> dependencyModuleUnit item == unit
    && dependencyModuleName item == name
    && not (dependencyModuleBoot item)
    && dependencyModuleProduct item == ProductReady)
    (dependencyModules evidence)
  source <- find ((== dependencyModuleSource node) . dependencySourcePath)
    (dependencySources evidence)
  pure (T.pack (dependencySourceSha256 source))

encodeGlobalWitness
  :: HscEnv -> IORef [PackageWitness] -> Map.Map SymbolIdentity BinderOwner
  -> Set.Set (T.Text, T.Text)
  -> SymbolIdentity -> RuntimeRep -> Maybe Signature -> Bool -> Maybe Word64
  -> IO (Either String Encoding)
encodeGlobalWitness env packageRef binders homeModules identity rep signature evaluated generation = do
  selected <- case generation of
    Just wanted -> pure (Right (array
      [encodeString "retained", encodeIdentity identity, encodeWord64 wanted]))
    Nothing -> case Map.lookup identity binders of
      Just (unit, name, version, ordinal) -> pure (Right (array
        [encodeString "source", encodeString unit, encodeString name
        , maybe encodeNull encodeString version, encodeWord ordinal
        , encodeIdentity identity]))
      Nothing
        | (symbolUnit identity, symbolModule identity) `Set.member` homeModules ->
            pure (Left "external home global has no certified source group")
        | otherwise -> packageOwner env packageRef identity
  pure $ do
    owner <- selected
    Right (array
      [ encodeIdentity identity, encodeRep rep
      , maybe encodeNull encodeSignature signature
      , encodeBool evaluated, owner ])

packageOwner :: HscEnv -> IORef [PackageWitness] -> SymbolIdentity
  -> IO (Either String Encoding)
packageOwner env packageRef identity = do
  selected <- resolvePackageGlobal env identity
  case selected of
    Left reason -> pure (Left reason)
    Right (_, witness) -> do
      let sha = T.pack (packageSha256 witness)
      modifyIORef' packageRef ((symbolUnit identity,
        symbolModule identity, packagePath witness, sha) :)
      pure (Right (array
        [encodeString "package", encodeString (symbolUnit identity)
        , encodeString (symbolModule identity), encodeString sha, encodeIdentity identity]))

-- Recovery and certification share one canonical package owner. The Id comes
-- from the exact interface's structural declaration, including implicit tops.
resolvePackageGlobal :: HscEnv -> SymbolIdentity
  -> IO (Either String (Id, PackageImportRoot))
resolvePackageGlobal env identity
  | symbolNamespace identity /= "value" =
      pure (Left (refusal "unsupported external global namespace"))
  | otherwise = do
      let owner = mkModule (stringToUnit (T.unpack (symbolUnit identity)))
            (mkModuleName (T.unpack (symbolModule identity)))
      found <- packageImportRoot env owner
      case found of
        Left reason -> pure (Left (refusal reason))
        Right witness -> do
          exact <- readExactInterface env owner
          selected <- case exact of
            Left _ -> pure (Failed ())
            Right (iface, location)
              | ml_hi_file location /= packagePath witness -> pure (Failed ())
              | otherwise -> canonicalPackageGlobal env owner iface
                  (mkVarOcc (T.unpack (symbolOccurrence identity)))
          case selected of
            Succeeded (AnId identifier) -> do
              unchanged <- validatePackageImportRoot env witness
              pure $ case unchanged of
                Left reason -> Left (refusal reason)
                Right () -> Right (identifier, witness)
            _ -> pure (Left (refusal "selected package global is absent from loaded interface"))
  where
    refusal reason = reason ++ ": " ++ T.unpack (symbolUnit identity) ++ ":"
      ++ T.unpack (symbolModule identity) ++ "." ++ T.unpack (symbolOccurrence identity)

-- The interface supplies canonical Names, including known-key bindings.
-- An implicit Id is authenticated by its defining parent declaration; wired
-- parents are authenticated by the exact interface's exported canonical Name.
-- Reconstructing a Name from Module/OccName can mint a different Unique.
canonicalPackageGlobal :: HscEnv -> Module -> ModIface -> OccName
  -> IO (MaybeErr () TyThing)
canonicalPackageGlobal env owner iface wanted = do
  let owned name = nameModule_maybe name == Just owner
      declarations =
        [ ifName declaration
        | (_, declaration) <- mi_decls iface
        , owned (ifName declaration)
        , nameOccName (ifName declaration) == wanted
            || wanted `elem` ifaceDeclImplicitBndrs declaration ]
      wiredParents = mapMaybe wiredInNameTyThing_maybe
        [ name | available <- mi_exports iface, name <- availNames available, owned name ]
  parents <- mapM loadCanonical declarations
  let things = [thing | Succeeded thing <- parents] ++ wiredParents
      candidates = Map.fromList
        [ (name, thing)
        | parent <- things
        , thing@(AnId _) <- parent : implicitTyThings parent
        , let name = getName thing
        , owned name, isVarOcc (nameOccName name), nameOccName name == wanted ]
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
  [ encodeString (productOrigin product)
  , encodeString (productUnit product), encodeString (productModule product)
  , maybe encodeNull encodeString (productVersion product)
  , encodeString (productSourceSha product)
  , encodeString (productIfaceSha product)
  , encodeString (productBytesSha product)
  , encodeString (productEvidenceSha product)
  , list (\(ordinal, globals) -> array [encodeWord ordinal, list id globals]) groups
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
