{-# LANGUAGE OverloadedStrings #-}

module Tidepool.ExactScope
  ( ExactScope(..), ExactProduct(..), ExactOriginalGroup(..), ExactCompilation(..), SourceSelectedOriginals(..)
  , CheckedCellAdmission(..), CheckedCellPurpose(..), CheckedItemAdmission(..), CheckedItemPurpose(..), CheckedDisplayAdmission(..)
  , PlannedCellAdmission(..), PlannedCellSlot(..)
  , ExactInterfaceEvidence(..), CanonicalInterfaceProof, CanonicalCoreArtifact
  , scopeCanonicalInterfaces
  , canonicalCertificatePath, canonicalCertificateSha256, canonicalCoreArtifact
  , canonicalCorePath, canonicalCoreSha256, canonicalHomeUnits, canonicalSourceSha256
  , canonicalRequirements
  , readExactScope, revalidateExactScope, scopeValueInterfaces
  , writeExactCompilation, extendSourceSelectedOriginals
  , extendExactExecutionSources, extendExactExecutionSourcesWithinBudget
  , scopeExecutionNativeOwners
  , originalGroupFromProjected, originalGroupFromCandidate
  ) where

import Codec.CBOR.Decoding
import Codec.CBOR.Read (deserialiseFromBytes)
import qualified Codec.CBOR.Encoding as E
import Codec.CBOR.Write (toStrictByteString)
import Control.Exception (IOException, try, throwIO, evaluate)
import Control.Monad (foldM, forM, forM_, replicateM, unless, when)
import qualified Crypto.Hash.SHA256 as SHA
import qualified Data.ByteString as BS
import qualified Data.ByteString.Lazy as BL
import Data.Char (isHexDigit)
import Data.List (isPrefixOf)
import qualified Data.Text as T
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import GHC.Driver.Env (HscEnv)
import Data.Word (Word64)
import Numeric (showHex)
import System.Directory (getFileSize, createDirectory, createDirectoryIfMissing, makeAbsolute, doesFileExist)
import System.FilePath (isAbsolute, takeDirectory, (</>))
import System.IO (IOMode(ReadMode), withBinaryFile)
import System.IO.Error (isAlreadyExistsError)
import Tidepool.ExactHydration (ExactIfaceArtifact(..))
import Tidepool.Session (SessionModule(..), SessionModuleKind(..), parseSessionModule, sessionModuleString)
import Tidepool.CheckedPrefixImports (CompletedValueImport(..))
import Tidepool.CheckedCell
  ( CheckedSignature(..), RequestTypeSignatures, RequestHelperRecipe(..), decodeCheckedSignature, decodeRequestTypeSignatures )
import Tidepool.ExecutionSchema
  ( SymbolIdentity(..), ProjectedGroup(..), ProjectedGroupBody(..), GlobalDecl(..) )
import Tidepool.ModuleCandidates (CandidateGroup(..), CandidateGlobal(..))
import Tidepool.ExecutionSource
  ( ExecutionSourceGraph(..), ExecutionSourceIdentity(..), ExecutionSourceOwner(..)
  , ExecutionSourceRef(..), ExecutionSourceNode(..), decodeExecutionSourceGraph, decodeExecutionSourceReferences
  , ExecutionSourceFailure(..), executionIdentityKey, executionSourceClosure, executionSourceOriginalNode
  , executionSourceOriginalClosure, executionSourceGraphBytesLimit )
import Tidepool.PackageWitness
  ( revalidatePackageImports )
import Tidepool.Timing (readTimingEnabled, timeDetailPhase, emitCount)
import Tidepool.DependencyEvidence
  ( DependencyEvidence(..), DependencySource(..), DependencyModule(..), DependencyImport(..), DependencyResolution(..), renderDependencyEvidence
  , DependencyQualifier, renderDependencyQualifier, revalidateDependencyEvidence )

data ExactScope = ExactScope
  { scopeManifestPath :: FilePath
  , scopeRequestSha256 :: String
  , scopeProducerSha256 :: String
  , scopeSemanticSha256 :: String
  , scopeInterfaces :: [(ExactIfaceArtifact, FilePath, String)]
  , scopeInterfaceEvidence :: Map.Map (String,String) ExactInterfaceEvidence
  , scopeLexical :: [((String, String), [(String, String)])]
  , scopeProducts :: [ExactProduct]
  , scopeExecutionGraphs :: [ExecutionSourceGraph]
  , scopeExecutionOwners :: [ExecutionSourceRef]
  , scopeCheckedCell :: Maybe CheckedCellAdmission
  , scopeCheckedItem :: Maybe CheckedItemAdmission
  , scopeCheckedDisplay :: Maybe CheckedDisplayAdmission
  , scopeCheckedInspection :: Maybe [ExactIfaceArtifact]
  , scopeIncludePaths :: Maybe [FilePath]
  , scopeRequestTypes :: Maybe (RequestHelperRecipe, RequestTypeSignatures)
  -- Request-local source proof roots are revalidated by subsequent stages;
  -- they are never serialized as baseline lexical authority.
  , scopeSourceSelectedOwners :: Set.Set (String,String)
  } deriving (Eq, Show)

-- Canonical proof belongs to its exact interface row. Core is a separate
-- compiler-input capability, never an imported declaration or native grant.
data CanonicalCoreArtifact = CanonicalCoreArtifact
  { canonicalCorePath :: FilePath
  , canonicalCoreSha256 :: String
  } deriving (Eq, Show)

data CanonicalInterfaceDescriptor = CanonicalInterfaceDescriptor
  { descriptorCertificatePath :: FilePath
  , descriptorCertificateSha256 :: String
  , descriptorCore :: Maybe CanonicalCoreArtifact
  } deriving (Eq, Show)

data CanonicalInterfaceProof = CanonicalInterfaceProof
  { canonicalCertificatePath :: FilePath
  , canonicalCertificateSha256 :: String
  , canonicalCoreArtifact :: Maybe CanonicalCoreArtifact
  , canonicalHomeUnits :: Set.Set String
  , canonicalSourceSha256 :: String
  , canonicalRequirements :: Map.Map (String,String) String
  } deriving (Eq, Show)

data ExactInterfaceEvidence
  = ModuleInterfaceEvidence CanonicalInterfaceProof
  | LexicalJoinEvidence
  | CheckedValueEvidence
  deriving (Eq, Show)

data ParsedInterfaceEvidence
  = ParsedModuleEvidence CanonicalInterfaceDescriptor
  | ParsedJoinEvidence
  | ParsedValueEvidence

scopeCanonicalInterfaces :: ExactScope -> Map.Map (String,String) CanonicalInterfaceProof
scopeCanonicalInterfaces = Map.mapMaybe select . scopeInterfaceEvidence
  where
    select (ModuleInterfaceEvidence proof) = Just proof
    select _ = Nothing

data CanonicalModuleCertificate = CanonicalModuleCertificate
  { certificateProducer :: String
  , certificateHomeUnits :: [String]
  , certificateOwner :: (String,String)
  , certificateSource :: String
  , certificateInterface :: String
  , certificatePackages :: String
  , certificateCore :: Maybe String
  , certificateRequirements :: [((String,String),String)]
  }

data CheckedDisplayAdmission = CheckedDisplayAdmission
  { displayAdmissionDigest :: String
  , displayCellReceiptDigest :: String
  , displayItemIndex :: Word64
  , displayObservationName :: String
  , displayCaptureGeneration :: Word64
  , displayGeneration :: Word64
  , displayPrefixDigest :: String
  , displayBudget :: Word64
  , displayPresented :: [String]
  , displayTurnTemplates :: [(String,String)]
  , displayInjectedModules :: [String]
  , displayValueImports :: [(String,[String])]
  , displayPresentation :: String
  , displayPlannedDeclaration :: Maybe ((String,String),String)
  , displayCompletedValues :: [CompletedValueImport]
  , displayValueInterfaces :: [ExactIfaceArtifact]
  } deriving (Eq, Show)

data CheckedCellPurpose = AuthoredCellCheck | HostInputCellCheck CheckedSignature
  deriving (Eq, Show)

data CheckedCellAdmission = CheckedCellAdmission
  { checkedAdmissionDigest :: String
  , checkedCellSha256 :: String
  , checkedTemplateSha256 :: String
  , checkedTurnTemplates :: [(String, String)]
  , checkedInjectedModules :: [String]
  , checkedReservedModules :: [String]
  , checkedValueInterfaces :: [ExactIfaceArtifact]
  , checkedPlannedCell :: Maybe PlannedCellAdmission
  , checkedCellPurpose :: CheckedCellPurpose
  } deriving (Eq, Show)

data PlannedCellSlot = PlannedPrologue Word64 | PlannedDeclaration Word64
  | PlannedBind Word64 | PlannedExpression Word64 Word64 String
  deriving (Eq, Show)

data PlannedCellAdmission = PlannedCellAdmission
  { plannedParserDigest :: String
  , plannedParserPath :: FilePath
  , plannedParserSha256 :: String
  , plannedReservationDigest :: String
  , plannedSlots :: [PlannedCellSlot]
  } deriving (Eq, Show)

data CheckedItemPurpose = AuthoredCheckedItem | HostActivationInput
  deriving (Eq, Show)

data CheckedItemAdmission = CheckedItemAdmission
  { itemPurpose :: CheckedItemPurpose
  , itemAdmissionDigest :: String
  , itemCellReceiptDigest :: String
  , itemIndex :: Word64
  , itemSourceDigest :: String
  , itemKind :: String
  , itemBinders :: [String]
  , itemTurnTemplates :: [(String,String)]
  , itemInjectedModules :: [String]
  , itemSignatures :: [CheckedSignature]
  , itemExpressionLift :: Maybe String
  , itemExpressionPresentation :: Maybe String
  , itemGeneration :: Word64
  , itemPrefixDigest :: String
  , itemValueImports :: [(String,[String])]
  , itemObservationName :: Maybe String
  , itemPlannedDeclaration :: Maybe ((String,String),String)
  , itemCompletedValues :: [CompletedValueImport]
  , itemValueInterfaces :: [ExactIfaceArtifact]
  } deriving (Eq, Show)

data ExactProduct = ExactProduct
  { originalUnit :: String, originalModule :: String
  , originalVersion :: String, originalIfaceSha256 :: String
  , originalProductSha256 :: String, originalProductPath :: FilePath
  , originalGroups :: [ExactOriginalGroup]
  } deriving (Eq, Show)

data ExactOriginalGroup = ExactOriginalGroup
  { originalOrdinal :: Word, originalBinders :: [SymbolIdentity]
  -- True imports need executable recovery; retained generations are boundaries.
  , originalGlobals :: [(SymbolIdentity, Bool)]
  } deriving (Eq, Show)

originalGroupFromProjected :: ProjectedGroup -> ExactOriginalGroup
originalGroupFromProjected group = ExactOriginalGroup
  (fromIntegral (projectedOriginalOrdinal group)) (projectedBinders group)
  [(globalIdentity global, globalRequiredGeneration global == Nothing)
   | global <- projectedGlobals (projectedBody group)]

originalGroupFromCandidate :: CandidateGroup -> ExactOriginalGroup
originalGroupFromCandidate group = ExactOriginalGroup
  (candidateGroupOrdinal group) (candidateGroupBinders group)
  [(candidateGlobalIdentity global, candidateGlobalGeneration global == Nothing)
   | global <- candidateGroupGlobals group]

-- Attach provenance after native originals have been promoted. Recipes can
-- supplement execution only for the exact native inventory already present;
-- neither their owner rows nor their dependency graph grants lexical imports.
extendExactExecutionSources :: [ExecutionSourceGraph] -> [ExecutionSourceRef]
  -> ExactScope -> Either ExecutionSourceFailure ExactScope
extendExactExecutionSources offeredGraphs offeredRefs scope = do
  admitted <- extendExactExecutionSourcesWithinBudget offeredGraphs offeredRefs scope
  maybe (Left (ExecutionSourceIncomplete ("","candidate execution parcel"))) Right admitted

-- Fresh transaction recipes are optional until demanded by a splice. Validate
-- all advertised originals before withholding a recipe at the parcel budget.
extendExactExecutionSourcesWithinBudget :: [ExecutionSourceGraph] -> [ExecutionSourceRef]
  -> ExactScope -> Either ExecutionSourceFailure (Maybe ExactScope)
extendExactExecutionSourcesWithinBudget offeredGraphs offeredRefs scope = do
  graphMap <- foldM insertGraph Map.empty (scopeExecutionGraphs scope ++ offeredGraphs)
  references <- foldM insertReference Map.empty (scopeExecutionOwners scope ++ offeredRefs)
  mapM_ (validateReference graphMap) (Map.elems references)
  forM_ offeredRefs $ \reference -> do
    _ <- executionSourceOriginalClosure (Map.elems graphMap) [reference]
    pure ()
  -- Current native admission can replace/reject a dependency while leaving
  -- its import ABI valid. Withhold that optional execution root transitively;
  -- the unchanged native product remains useful, and a later demanded splice
  -- still refuses the missing capability.
  available <- fmap concat $ mapM (\reference ->
    case executionSourceClosure (Map.elems graphMap) (Map.elems references) (scopeExecutionNativeOwners scope)
        [executionIdentityKey (executionRefIdentity reference)] of
      Right closureNodes -> Right [(reference,closureNodes)]
      Left (ExecutionSourceUnavailable _) -> Right []
      Left refusal -> Left refusal) offeredRefs
  let nodes = concatMap snd available
  let selectedRefs = [reference | node <- nodes
        , Just reference <- [Map.lookup (executionIdentityKey (executionNodeIdentity node)) references]]
  needed <- foldM (retainGraph graphMap) Set.empty
    [(executionRefIdentity reference,executionRefGraph reference) | reference <- selectedRefs]
  retainedReferences <- foldM insertReference Map.empty (scopeExecutionOwners scope ++ map fst available)
  let kept = Set.union (Set.map snd needed) (Set.fromList (map executionGraphSha256 (scopeExecutionGraphs scope)))
      graphs = [graph | (sha,graph) <- Map.toAscList graphMap, sha `Set.member` kept]
  if length graphs <= 4096 && Map.size references <= 4096
      && sum (map (BS.length . executionGraphBytes) graphs) <= executionSourceGraphBytesLimit
    then pure (Just scope {scopeExecutionGraphs=graphs,scopeExecutionOwners=Map.elems retainedReferences})
    else pure Nothing
  where
    insertGraph selected graph = do
      unless (executionGraphProducer graph == scopeProducerSha256 scope)
        (Left (ExecutionSourceConflicting ("",executionGraphSha256 graph)))
      case Map.lookup (executionGraphSha256 graph) selected of
        Nothing -> Right (Map.insert (executionGraphSha256 graph) graph selected)
        Just previous | executionGraphBytes previous == executionGraphBytes graph -> Right selected
        _ -> Left (ExecutionSourceConflicting ("",executionGraphSha256 graph))
    insertReference selected reference =
      let key = executionIdentityKey (executionRefIdentity reference)
      in case Map.lookup key selected of
        Nothing -> Right (Map.insert key reference selected)
        Just previous | previous == reference -> Right selected
        _ -> Left (ExecutionSourceConflicting key)
    validateReference graphs reference = do
      let originalIdentity = executionRefIdentity reference
          key = executionIdentityKey originalIdentity
          matched product' = (originalUnit product',originalModule product') == key
            && originalVersion product' == executionVersion originalIdentity
            && originalIfaceSha256 product' == executionIfaceSha256 originalIdentity
            && originalProductSha256 product' == executionNativeSha256 originalIdentity
      unless (any matched (scopeProducts scope)
          && any (\(iface,_,_) -> (exactUnit iface,exactModule iface) == key
            && exactSha256 iface == executionIfaceSha256 originalIdentity) (scopeInterfaces scope))
        (Left (ExecutionSourceConflicting key))
      graph <- maybe (Left (ExecutionSourceMissing key)) Right
        (Map.lookup (executionRefGraph reference) graphs)
      unless (executionGraphProducer graph == scopeProducerSha256 scope
          && any ((== originalIdentity) . executionOwnerIdentity) (executionGraphOwners graph))
        (Left (ExecutionSourceConflicting key))
      _ <- executionSourceOriginalNode (Map.elems graphs) originalIdentity (executionRefGraph reference)
      pure ()
    retainGraph graphs selected (originalIdentity,sha)
      | (originalIdentity,sha) `Set.member` selected = Right selected
      | otherwise = do
          let key = executionIdentityKey originalIdentity
          graph <- maybe (Left (ExecutionSourceMissing key)) Right (Map.lookup sha graphs)
          unless (executionGraphProducer graph == scopeProducerSha256 scope)
            (Left (ExecutionSourceConflicting key))
          foldM (retainGraph graphs) (Set.insert (originalIdentity,sha) selected)
            [(originalIdentity,original) | graphOwner <- executionGraphOwners graph
              , executionOwnerIdentity graphOwner == originalIdentity
              , Just original <- [executionOwnerOriginalGraph graphOwner]]

scopeExecutionNativeOwners :: ExactScope -> [ExecutionSourceIdentity]
scopeExecutionNativeOwners scope =
  [ExecutionSourceIdentity (originalUnit product') (originalModule product')
    (originalVersion product') (originalIfaceSha256 product') (originalProductSha256 product')
  | product' <- scopeProducts scope
  , any (\(iface,_,_) -> (exactUnit iface,exactModule iface) == (originalUnit product',originalModule product')
      && exactSha256 iface == originalIfaceSha256 product') (scopeInterfaces scope)]

-- Current source-import authority is separate from retained native ownership.
-- Each row names the ultimate fresh recipe authenticated by that original.
data SourceSelectedOriginals = SourceSelectedOriginals
  { selectedOriginalRows :: [(ExecutionSourceIdentity,String)]
  , selectedOriginalEvidence :: DependencyEvidence
  }

instance Eq SourceSelectedOriginals where
  left == right = selectedOriginalRows left == selectedOriginalRows right
    && renderDependencyEvidence (selectedOriginalEvidence left)
      == renderDependencyEvidence (selectedOriginalEvidence right)

instance Show SourceSelectedOriginals where
  show selected = "SourceSelectedOriginals " ++ show (selectedOriginalRows selected)

extendSourceSelectedOriginals :: Maybe SourceSelectedOriginals -> ExactScope -> Either String ExactScope
extendSourceSelectedOriginals Nothing scope = Right scope
extendSourceSelectedOriginals (Just selected) scope = do
  let rows = selectedOriginalRows selected
      selectedKeys = Set.fromList (map (executionIdentityKey . fst) rows)
      nodes = dependencyModules (selectedOriginalEvidence selected)
      byKey = Map.fromList [((dependencyModuleUnit node,dependencyModuleName node),node) | node <- nodes]
      available = Map.fromList [((exactUnit artifact,exactModule artifact),artifact) | (artifact,_,_) <- scopeInterfaces scope]
      existing = Map.fromList (scopeLexical scope)
      adjacency node = Set.toAscList (Set.fromList
        [(dependencyModuleUnit node,dependencyImportName edge)
        | edge <- dependencyModuleImports node, dependencyImportSelected edge /= Nothing])
  unless (length rows == Set.size selectedKeys && Map.keysSet byKey == selectedKeys)
    (Left "source selection owner rows differ from their matched sources")
  lexical <- forM rows $ \(original,_) -> do
    let key = executionIdentityKey original
    artifact <- maybe (Left "source-selected original lacks exact interface") Right (Map.lookup key available)
    let native = [originalProduct | originalProduct <- scopeProducts scope
          , (originalUnit originalProduct,originalModule originalProduct) == key
          , originalVersion originalProduct == executionVersion original
          , originalIfaceSha256 originalProduct == executionIfaceSha256 original
          , originalProductSha256 originalProduct == executionNativeSha256 original]
    unless (exactSha256 artifact == executionIfaceSha256 original
        && not ("Tidepool.Session." `isPrefixOf` snd key) && length native == 1)
      (Left "source-selected original has another exact interface/native owner")
    node <- maybe (Left "source-selected original lacks source adjacency") Right (Map.lookup key byKey)
    let imports = adjacency node
    unless (all (`Set.member` Set.union selectedKeys (Map.keysSet existing)) imports)
      (Left "source-selected adjacency leaves its admitted source graph")
    forM_ (Map.lookup key existing) $ \old -> unless (old == imports)
      (Left "source-selected original changed inherited lexical adjacency")
    pure (key,imports)
  pure scope
    { scopeLexical=Map.toAscList (Map.union (Map.fromList lexical) existing)
    , scopeSourceSelectedOwners=Set.union selectedKeys (scopeSourceSelectedOwners scope) }

data ExactCompilation = ExactCompilation
  { compilationScope :: ExactScope
  , compilationTransaction :: Word64
  , compilationSource :: FilePath
  , compilationImports :: [((String, String, Bool), [(DependencyQualifier, String, Bool, String)])]
  , compilationSourceSelection :: Maybe SourceSelectedOriginals
  } deriving (Eq, Show)

scopeValueInterfaces :: ExactScope -> [ExactIfaceArtifact]
scopeValueInterfaces scope =
  maybe [] checkedValueInterfaces (scopeCheckedCell scope)
    ++ maybe [] itemValueInterfaces (scopeCheckedItem scope)
    ++ maybe [] displayValueInterfaces (scopeCheckedDisplay scope)
    ++ maybe [] id (scopeCheckedInspection scope)

-- Scope v8 separates the bounded metadata envelope from the independently
-- bounded original graph bytes. The request hash seals each path and digest.
readExactScope :: FilePath -> IO (Either String ExactScope)
readExactScope path = do
  timing <- readTimingEnabled
  timeDetailPhase timing "exact_scope" "read" $ do
    captured <- try (do
      unless (isAbsolute path) (fail "exact scope path must be absolute")
      bytes <- readBoundedFile path (4 * 1024 * 1024)
      (scope, descriptors, interfaceEvidence) <- timeDetailPhase timing "exact_scope" "decode" $ case deserialiseFromBytes decodeScope (BL.fromStrict bytes) of
        Left failure -> fail (show failure)
        Right (remaining, result)
          | BL.null remaining -> pure result
          | otherwise -> fail "exact scope has trailing bytes"
      sizes <- forM descriptors $ \(_, graphPath) -> do
        unless (takeDirectory graphPath == takeDirectory path)
          (fail "original execution graph is outside its request directory")
        getFileSize graphPath
      when (sum sizes > fromIntegral executionSourceGraphBytesLimit)
        (fail "original execution graphs exceed 64 MiB")
      graphs <- forM (zip descriptors sizes) $ \((sha, graphPath), size) -> do
        graphBytes <- readBoundedFile graphPath (fromIntegral size)
        unless (toInteger (BS.length graphBytes) == size)
          (fail "original execution graph size changed")
        graph <- either fail pure (decodeExecutionSourceGraph sha graphBytes)
        emitCount timing ("hash_bytes.execution_graph." ++ sha) (fromIntegral (BS.length graphBytes))
        pure graph
      evidence <- validateInterfaceEvidence scope interfaceEvidence
      validateExecutionSources scope graphs
      let sha = digest bytes
      when timing $ do
        _ <- evaluate (length sha)
        emitCount timing ("hash_bytes.scope_metadata." ++ sha) (fromIntegral (BS.length bytes))
      pure scope { scopeManifestPath = path, scopeRequestSha256 = sha
        , scopeExecutionGraphs = graphs, scopeInterfaceEvidence = evidence }) :: IO (Either IOException ExactScope)
    pure (either (Left . show) Right captured)

-- A bounded read also closes the stat/read growth race without allocating an
-- unbounded input. Graph sizes are summed before any graph is captured.
readBoundedFile :: FilePath -> Int -> IO BS.ByteString
readBoundedFile path limit = withBinaryFile path ReadMode $ \handle -> do
  bytes <- BS.hGet handle (limit + 1)
  when (BS.length bytes > limit) (fail "exact scope artifact exceeds its byte bound")
  pure bytes

validateExecutionSources :: ExactScope -> [ExecutionSourceGraph] -> IO ()
validateExecutionSources scope graphs = do
  forM_ graphs $ \graph -> unless (executionGraphProducer graph == scopeProducerSha256 scope)
    (fail "original execution graph has another compiler producer")
  forM_ (scopeExecutionOwners scope) $ \reference -> do
    let original = executionRefIdentity reference
        matchingProduct product' = originalUnit product' == executionUnit original
          && originalModule product' == executionModule original
          && originalVersion product' == executionVersion original
          && originalIfaceSha256 product' == executionIfaceSha256 original
          && originalProductSha256 product' == executionNativeSha256 original
        matchingGraph graph = executionGraphSha256 graph == executionRefGraph reference
          && any ((== original) . executionOwnerIdentity) (executionGraphOwners graph)
    unless (any matchingProduct (scopeProducts scope) && any matchingGraph graphs)
      (fail "original execution reference leaves its admitted native owner")

-- Validate canonical certificates before any interface hydration. The complete
-- home-unit census is producer evidence, not a classification inferred from the
-- retained subset. Core bytes are loaded only by their demanding recovery owner.
validateInterfaceEvidence
  :: ExactScope -> [((String,String),ParsedInterfaceEvidence)]
  -> IO (Map.Map (String,String) ExactInterfaceEvidence)
validateInterfaceEvidence scope offered = do
  proofs <- validateCanonicalInterfaces scope
    [(key,descriptor) | (key,ParsedModuleEvidence descriptor) <- offered]
  let evidence = Map.fromList [(key,case value of
        ParsedModuleEvidence _ -> ModuleInterfaceEvidence (proofs Map.! key)
        ParsedJoinEvidence -> LexicalJoinEvidence
        ParsedValueEvidence -> CheckedValueEvidence) | (key,value) <- offered]
  unless (Map.keysSet evidence == Set.fromList
      [(exactUnit iface,exactModule iface) | (iface,_,_) <- scopeInterfaces scope]
      && all (\product' -> Map.member (originalUnit product',originalModule product') proofs)
        (scopeProducts scope))
    (fail "exact interface evidence is incomplete or lacks native module proof")
  pure evidence

validateCanonicalInterfaces
  :: ExactScope -> [((String,String),CanonicalInterfaceDescriptor)]
  -> IO (Map.Map (String,String) CanonicalInterfaceProof)
validateCanonicalInterfaces scope descriptors = do
  let interfaces = Map.fromList
        [((exactUnit iface,exactModule iface),(iface,packages,packageSha))
        | (iface,packages,packageSha) <- scopeInterfaces scope]
  proofs <- forM descriptors $ \(key,descriptor) -> do
    (iface,packages,packageSha) <- maybe (fail "canonical proof has no exact interface") pure
      (Map.lookup key interfaces)
    bytes <- readBoundedFile (descriptorCertificatePath descriptor) (4 * 1024 * 1024)
    unless (digest bytes == descriptorCertificateSha256 descriptor)
      (fail "canonical module certificate changed")
    certificate <- case deserialiseFromBytes decodeCanonicalModuleCertificate (BL.fromStrict bytes) of
      Left failure -> fail (show failure)
      Right (remaining,value)
        | BL.null remaining -> pure value
        | otherwise -> fail "canonical module certificate has trailing bytes"
    unless (toStrictByteString (encodeCanonicalModuleCertificate certificate) == bytes)
      (fail "noncanonical module certificate encoding")
    unless (certificateProducer certificate == scopeProducerSha256 scope
        && certificateOwner certificate == key
        && certificateInterface certificate == exactSha256 iface
        && certificatePackages certificate == packageSha
        && certificateCore certificate == (canonicalCoreSha256 <$> descriptorCore descriptor))
      (fail "canonical module certificate differs from exact owner or payload")
    let requirements = Map.fromList (certificateRequirements certificate)
    let matchesRequirement (required,seal) = case Map.lookup required interfaces of
          Just (value,_,_) -> exactSha256 value == seal
          Nothing -> False
    unless (Map.keysSet requirements == Set.fromList (exactRequirements iface)
        && all matchesRequirement (Map.toAscList requirements))
      (fail "canonical module requirements differ from selected exact interfaces")
    interfaceBytes <- readBoundedFile (exactPath iface) (32 * 1024 * 1024)
    packageBytes <- readBoundedFile packages (4 * 1024 * 1024)
    unless (digest interfaceBytes == certificateInterface certificate
        && digest packageBytes == certificatePackages certificate)
      (fail "canonical module interface or package imports changed")
    pure (key, CanonicalInterfaceProof
      { canonicalCertificatePath = descriptorCertificatePath descriptor
      , canonicalCertificateSha256 = descriptorCertificateSha256 descriptor
      , canonicalCoreArtifact = descriptorCore descriptor
      , canonicalHomeUnits = Set.fromList (certificateHomeUnits certificate)
      , canonicalSourceSha256 = certificateSource certificate
      , canonicalRequirements = requirements
      })
  pure (Map.fromList proofs)

decodeCanonicalModuleCertificate :: Decoder s CanonicalModuleCertificate
decodeCanonicalModuleCertificate = do
  array 12
  magic <- string
  version <- decodeWord
  profile <- string
  unless (magic == "TPFINALMODULE" && version == 1
      && profile == "tidepool-ghc-finalized-module-v1")
    (fail "unsupported canonical module certificate")
  producer <- canonicalDigest
  homes <- bounded 128 nonempty
  unless (not (null homes) && and (zipWith (<) homes (drop 1 homes)))
    (fail "invalid complete home unit inventory")
  key@(unit,_) <- (,) <$> nonempty <*> nonempty
  source <- canonicalDigest
  interface <- canonicalDigest
  packages <- canonicalDigest
  token <- peekTokenType
  core <- if token == TypeNull then decodeNull >> pure Nothing else Just <$> canonicalDigest
  requirements <- bounded 128 (array 3 >> ((,) <$> ((,) <$> nonempty <*> nonempty) <*> canonicalDigest))
  unless (and (zipWith (<) (map fst requirements) (drop 1 (map fst requirements)))
      && unit `elem` homes && all ((`elem` homes) . fst . fst) requirements
      && key `notElem` map fst requirements)
    (fail "invalid canonical module requirement inventory")
  pure (CanonicalModuleCertificate producer homes key source interface packages core requirements)

encodeCanonicalModuleCertificate :: CanonicalModuleCertificate -> E.Encoding
encodeCanonicalModuleCertificate certificate = E.encodeListLen 12
  <> text "TPFINALMODULE" <> E.encodeWord 1 <> text "tidepool-ghc-finalized-module-v1"
  <> text (certificateProducer certificate)
  <> list text (certificateHomeUnits certificate)
  <> text (fst (certificateOwner certificate)) <> text (snd (certificateOwner certificate))
  <> text (certificateSource certificate) <> text (certificateInterface certificate)
  <> text (certificatePackages certificate)
  <> maybe E.encodeNull text (certificateCore certificate)
  <> list (\((unit,name),seal) -> E.encodeListLen 3 <> text unit <> text name <> text seal)
      (certificateRequirements certificate)
  where
    text = E.encodeString . T.pack
    list encode values = E.encodeListLen (fromIntegral (length values)) <> foldMap encode values

canonicalDigest :: Decoder s String
canonicalDigest = do
  value <- digestField
  unless (value /= replicate 64 '0' && all (`elem` ("0123456789abcdef" :: String)) value)
    (fail "invalid canonical digest")
  pure value

-- Recheck the entire producer-owned closure in the consuming transaction;
-- no source file is a substitute for an admitted original interface.
revalidateExactScope :: HscEnv -> ExactScope -> IO (Either String ())
revalidateExactScope env scope = do
  timing <- readTimingEnabled
  timeDetailPhase timing "exact_scope" "revalidate" $ do
    result <- try (do
      bytes <- readBoundedFile (scopeManifestPath scope) (4 * 1024 * 1024)
      unless (digest bytes == scopeRequestSha256 scope) (fail "exact scope request changed")
      evidence <- validateInterfaceEvidence scope
        [(key,case value of
          ModuleInterfaceEvidence proof -> ParsedModuleEvidence
            (CanonicalInterfaceDescriptor (canonicalCertificatePath proof)
              (canonicalCertificateSha256 proof) (canonicalCoreArtifact proof))
          LexicalJoinEvidence -> ParsedJoinEvidence
          CheckedValueEvidence -> ParsedValueEvidence)
        | (key,value) <- Map.toAscList (scopeInterfaceEvidence scope)]
      unless (evidence == scopeInterfaceEvidence scope)
        (fail "exact interface evidence changed")
      revalidatePackageImports env (scopeInterfaces scope) >>= either fail pure
      emitCount timing ("hash_bytes.scope_revalidation." ++ scopeRequestSha256 scope) (fromIntegral (BS.length bytes))
      mapM_ (checkProduct timing) (scopeProducts scope)
      mapM_ (checkValue timing) (scopeValueInterfaces scope))
      :: IO (Either IOException ())
    pure $ either (Left . show) Right result
  where
    checkValue timing value = do
      bytes <- BS.readFile (exactPath value)
      unless (digest bytes == exactSha256 value) (fail "checked value interface changed")
      emitCount timing ("hash_bytes.checked_value." ++ exactSha256 value) (fromIntegral (BS.length bytes))
    checkProduct timing originalProduct = do
      bytes <- BS.readFile (originalProductPath originalProduct)
      unless (digest bytes == originalProductSha256 originalProduct)
        (fail "exact original product changed")
      emitCount timing ("hash_bytes.native_product." ++ originalProductSha256 originalProduct) (fromIntegral (BS.length bytes))

-- Every successful compile owns a distinct immutable source snapshot. Check,
-- fold and inspection requests can consume several generated modules, so a
-- later successful transaction must not replace an earlier witness.
writeExactCompilation
  :: ExactCompilation -> DependencyEvidence -> IO ()
writeExactCompilation compilation evidence = do
  let scope = compilationScope compilation
      transaction = compilationTransaction compilation
      source = compilationSource compilation
      imports = compilationImports compilation
  path <- makeAbsolute source
  bytes <- BS.readFile path
  unless (any (\item -> dependencySourcePath item == path
      && dependencySourceSha256 item == digest bytes) (dependencySources evidence))
    (fail "exact compile source differs from consumed source")
  unchanged <- revalidateDependencyEvidence evidence
  unless unchanged (fail "exact compile consumed source changed before receipt")
  forM_ (compilationSourceSelection compilation) $ \selected -> do
    unchangedSelection <- revalidateDependencyEvidence (selectedOriginalEvidence selected)
    negativeSelection <- and <$> forM (dependencyResolutions (selectedOriginalEvidence selected)) (\resolution -> do
      let absent = case dependencyResolutionSelected resolution of
            Nothing -> dependencyResolutionCandidates resolution
            Just chosen -> takeWhile (/= chosen) (dependencyResolutionCandidates resolution)
      and <$> mapM (fmap not . doesFileExist) absent)
    unless (unchangedSelection && negativeSelection)
      (fail "current original source selection changed before receipt")
  let parent = takeDirectory path </> ".exact-compilations"
  createDirectoryIfMissing True parent
  directory <- reserveCompilationDirectory parent transaction
  let snapshot = directory </> "source.hs"
      encodeArray values = E.encodeListLen (fromIntegral (length values)) <> mconcat values
      text = E.encodeString . T.pack
      importRow (qualifier, name, boot, unit) = encodeArray
        [text (renderDependencyQualifier qualifier), text name, E.encodeBool boot, text unit]
      moduleRow ((unit, name, boot), edges) = encodeArray
        [text unit, text name, E.encodeBool boot, encodeArray (map importRow edges)]
      receipt = encodeArray
        [text "TPEXACTCOMPILE", text "2", text (scopeRequestSha256 scope)
        , text (scopeSemanticSha256 scope), text path, text (digest bytes)
        , text snapshot, text (renderDependencyEvidence evidence)
        , encodeArray (map moduleRow imports)
        , case compilationSourceSelection compilation of
            Nothing -> encodeArray [encodeArray [], E.encodeNull]
            Just selected -> encodeArray
              [ encodeArray [encodeArray [text (executionUnit original),text (executionModule original)
                    ,text (executionVersion original),text (executionIfaceSha256 original)
                    ,text (executionNativeSha256 original),text graph]
                  | (original,graph) <- selectedOriginalRows selected]
              , text (renderDependencyEvidence (selectedOriginalEvidence selected))]]
  BS.writeFile snapshot bytes
  BS.writeFile (directory </> "receipt.cbor") (toStrictByteString receipt)

-- One worker request can check, refine and compile several sources. The
-- request identity correlates diagnostics; it cannot identify one immutable
-- compilation snapshot. Atomic directory creation also keeps concurrent
-- writers from replacing an earlier successful receipt.
reserveCompilationDirectory :: FilePath -> Word64 -> IO FilePath
reserveCompilationDirectory parent transaction = attempt (0 :: Int)
  where
    attempt ordinal
      | ordinal >= 4096 = fail "excessive exact compilations in one request"
      | otherwise = do
          let path = parent </> (show transaction ++ "-" ++ show ordinal)
          reserved <- try (createDirectory path) :: IO (Either IOException ())
          case reserved of
            Right () -> pure path
            Left failure | isAlreadyExistsError failure -> attempt (ordinal + 1)
            Left failure -> throwIO failure

decodeScope :: Decoder s (ExactScope, [(String, FilePath)], [((String,String),ParsedInterfaceEvidence)])
decodeScope = do
  count <- decodeListLen
  magic <- string
  version <- string
  unless (magic == "TPEXACTSCOPE" && version == "8" && count == 9)
    (fail "unsupported exact scope")
  semantic <- digestField
  producer <- digestField
  interfaceRows <- bounded 4096 $ do
    array 8
    unit <- nonempty
    name <- nonempty
    path <- absolute
    sha <- digestField
    requirements <- bounded 4096 owner
    packages <- absolute
    packageSha <- digestField
    unique "exact requirements" requirements
    evidenceCount <- decodeListLen
    evidenceRole <- string
    evidence <- case (evidenceRole,evidenceCount) of
      ("module",5) -> do
        certificatePath <- absolute
        certificateSha <- canonicalDigest
        coreToken <- peekTokenType
        core <- if coreToken == TypeNull then do
          decodeNull
          decodeNull
          pure Nothing
          else Just <$> (CanonicalCoreArtifact <$> absolute <*> canonicalDigest)
        pure (ParsedModuleEvidence (CanonicalInterfaceDescriptor certificatePath certificateSha core))
      ("join",1) -> pure ParsedJoinEvidence
      ("value",1) -> pure ParsedValueEvidence
      _ -> fail "unsupported exact interface evidence role"
    pure ((ExactIfaceArtifact unit name path sha requirements, packages, packageSha),evidence)
  let interfaces = map fst interfaceRows
      interfaceEvidence = [((exactUnit iface,exactModule iface),evidence)
        | ((iface,_,_),evidence) <- interfaceRows]
      canonicalOwners = [key | (key,ParsedModuleEvidence _) <- interfaceEvidence]
  lexical <- bounded 4096 $ do
    array 2
    node <- owner
    imports <- bounded 4096 owner
    unique "exact lexical imports" imports
    pure (node, imports)
  products <- bounded 4096 $ do
    array 7
    originalProduct <- ExactProduct <$> nonempty <*> nonempty <*> digestField
      <*> digestField <*> digestField <*> absolute
      <*> bounded 65536 (do
        array 3
        ExactOriginalGroup <$> decodeWord <*> bounded 65536 identity
          <*> bounded 65536 (array 2 >> (,) <$> identity <*> decodeBool))
    unique "exact original ordinals" (map originalOrdinal (originalGroups originalProduct))
    let binders = concatMap originalBinders (originalGroups originalProduct)
    unique "exact original binders" binders
    unless (all (\binder -> T.unpack (symbolUnit binder) == originalUnit originalProduct
        && T.unpack (symbolModule binder) == originalModule originalProduct) binders)
      (fail "exact binder has another original owner")
    pure originalProduct
  let keys = [(exactUnit iface, exactModule iface) | (iface, _, _) <- interfaces]
      selected = map fst lexical
      productKeys = [(originalUnit originalProduct, originalModule originalProduct) | originalProduct <- products]
  unique "exact interface owners" keys
  unique "exact module names" (map snd keys)
  unique "exact lexical owners" selected
  unique "exact product owners" productKeys
  unless (all (`elem` keys) selected
      && all (`elem` selected) (concatMap snd lexical)
      && all (`elem` keys) productKeys
      && all (`elem` canonicalOwners) productKeys
      && all (\(iface, _, _) -> all (`elem` keys) (exactRequirements iface)) interfaces
      && all (\originalProduct -> any (\(iface, _, _) ->
          (exactUnit iface, exactModule iface) == (originalUnit originalProduct, originalModule originalProduct)
          && exactSha256 iface == originalIfaceSha256 originalProduct) interfaces) products)
    (fail "incomplete or conflicting exact owner closure")
  executionToken <- peekTokenType
  (descriptors, executionOwners) <- if executionToken /= TypeNull then do
    array 2
    graphs <- bounded 4096 (array 2 >> (,) <$> digestField <*> absolute)
    references <- decodeExecutionSourceReferences
    unique "original execution graphs" (map fst graphs)
    pure (graphs, references)
    else decodeNull >> pure ([], [])
  nullPurpose <- (== TypeNull) <$> peekTokenType
  (requestTypes, (checked, checkedItem, checkedDisplay, inspectionValues, includes)) <- if nullPurpose
    then do
      when nullPurpose decodeNull
      pure (Nothing, (Nothing,Nothing,Nothing,Nothing,Nothing))
    else do
    outerCount <- decodeListLen
    outerPurpose <- string
    (requestTypes, purpose) <- if outerPurpose == "request-types2" then do
      unless (outerCount == 4) (fail "invalid native request type admission")
      native <- decodeRequestTypeSignatures
      recipe <- string >>= \tag -> case tag of
        "none" -> pure NoRequestHelpers
        "actor-reply" -> pure ActorReplyHelpers
        _ -> fail "unsupported request helper recipe"
      token <- peekTokenType
      inner <- if token == TypeNull then decodeNull >> pure Nothing
        else Just <$> ((,) <$> decodeListLen <*> string)
      pure (Just (recipe, native), inner)
      else pure (Nothing, Just (outerCount, outerPurpose))
    admission <- maybe (pure (Nothing,Nothing,Nothing,Nothing,Nothing))
      (uncurry decodePurpose) purpose
    pure (requestTypes, admission)
  pure (ExactScope "" "" producer semantic interfaces Map.empty lexical products [] executionOwners
    checked checkedItem checkedDisplay inspectionValues includes requestTypes Set.empty, descriptors, interfaceEvidence)
  where
    decodePurpose authCount purpose = case purpose of
      "inspection1" -> do
        unless (authCount == 4) (fail "invalid inspection admission")
        injected <- bounded 4096 nonempty
        values <- valueInterfaces
        unique "inspection injected modules" injected
        validateInterfaces injected values
        paths <- includePaths
        pure (Nothing,Nothing,Nothing,Just values,Just paths)
      tag | tag == "cell-check2" || tag == "host-input-check1" -> do
        unless (authCount == if tag == "host-input-check1" then 10 else 9) (fail "invalid cell-check admission")
        admission <- CheckedCellAdmission <$> digestField <*> digestField <*> digestField
          <*> bounded 64 (array 2 >> (,) <$> nonempty <*> digestField)
          <*> bounded 4096 nonempty <*> bounded 4096 nonempty <*> valueInterfaces <*> pure Nothing
          <*> (if tag == "host-input-check1" then HostInputCellCheck <$> signature else pure AuthoredCellCheck)
        case checkedCellPurpose admission of
          HostInputCellCheck input -> unless (signatureKey input == "activation-input"
            && null (checkedReservedModules admission) && map fst (checkedTurnTemplates admission) == ["bind"])
              (fail "invalid host input check admission")
          AuthoredCellCheck -> pure ()
        validateInterfaces (checkedInjectedModules admission) (checkedValueInterfaces admission)
        unique "checked injected modules" (checkedInjectedModules admission)
        unique "checked reserved modules" (checkedReservedModules admission)
        paths <- includePaths
        pure (Just admission,Nothing,Nothing,Nothing,Just paths)
      "cell-program1" -> do
        unless (authCount == 14) (fail "invalid compiled cell admission")
        admission <- CheckedCellAdmission <$> digestField <*> digestField <*> digestField
          <*> bounded 64 (array 2 >> (,) <$> nonempty <*> digestField)
          <*> bounded 4096 nonempty <*> bounded 4096 nonempty <*> valueInterfaces
          <*> (Just <$> (PlannedCellAdmission <$> digestField <*> absolute <*> digestField <*> digestField
            <*> bounded 10000 (do
              slotFields <- decodeListLen
              kind <- nonempty
              case (kind,slotFields) of
                ("prologue",2) -> PlannedPrologue <$> decodeWord64
                ("decl",2) -> PlannedDeclaration <$> decodeWord64
                ("bind",2) -> PlannedBind <$> decodeWord64
                ("expr",4) -> PlannedExpression <$> decodeWord64 <*> decodeWord64 <*> nonempty
                _ -> fail "invalid compiled cell reservation")))
          <*> pure AuthoredCellCheck
        validateInterfaces (checkedInjectedModules admission) (checkedValueInterfaces admission)
        paths <- includePaths
        pure (Just admission,Nothing,Nothing,Nothing,Just paths)
      tag | tag == "checked-item2" || tag == "host-activation-input1" -> do
        unless (authCount == 19) (fail "invalid checked-item admission")
        admissionDigest <- digestField
        receiptDigest <- digestField
        index <- decodeWord64
        sourceDigest <- digestField
        kind <- nonempty
        unless (kind `elem` ["bind","expr"]) (fail "unsupported checked-item kind")
        binders <- bounded 65536 nonempty
        templates <- bounded 64 (array 2 >> (,) <$> nonempty <*> digestField)
        injected <- bounded 4096 nonempty
        signatures <- bounded 65536 signature
        unique "checked item binders" binders
        unique "checked item injected modules" injected
        unique "checked item signatures" (map signatureKey signatures)
        token <- peekTokenType
        (liftPlan,presentation) <- if token == TypeNull then decodeNull >> pure (Nothing,Nothing) else do
          array 6
          _key <- nonempty
          liftPlan <- nonempty
          presentation <- nonempty
          unless (kind == "expr" && liftPlan `elem` ["pure","effectful"] && presentation `elem` ["rendered","opaque"])
            (fail "invalid checked expression plan")
          _rendering <- string
          _heads <- bounded 65536 (array 3 >> ((,,) <$> nonempty <*> nonempty <*> nonempty))
          _imports <- bounded 4096 nonempty
          pure (Just liftPlan,Just presentation)
        generation <- decodeWord64
        prefix <- digestField
        valueImports <- bounded 4096 (array 2 >> ((,) <$> nonempty <*> bounded 65536 nonempty))
        unique "completed value import owners" (map fst valueImports)
        unique "completed value import names" (concatMap snd valueImports)
        unless (all ((`elem` injected) . fst) valueImports) (fail "completed value import has no exact injected owner")
        observationToken <- peekTokenType
        observation <- if observationToken == TypeNull then decodeNull >> pure Nothing else Just <$> nonempty
        unless ((kind == "expr") == maybe False (const True) observation)
          (fail "checked observation identity differs from item kind")
        planned <- plannedDeclaration
        values <- completedValues
        valueInputs <- valueInterfaces
        validateInterfaces injected valueInputs
        validateValues valueImports values
        let role = if tag == "host-activation-input1" then HostActivationInput else AuthoredCheckedItem
        when (role == HostActivationInput) $
          unless (index == 0 && kind == "bind" && binders == ["sessionInput"]
              && generation > 0
              && all (/= replicate 64 '0') [admissionDigest,receiptDigest,prefix]
              && map fst templates == ["bind"]
              && map signatureKey signatures == ["__tidepool_cell_pin_0_sessionInput"]
              && liftPlan == Nothing && presentation == Nothing && observation == Nothing)
            (fail "invalid host activation input admission")
        paths <- includePaths
        pure (Nothing, Just (CheckedItemAdmission role admissionDigest receiptDigest index sourceDigest kind binders
          templates injected signatures liftPlan presentation generation prefix valueImports observation planned values valueInputs), Nothing, Nothing, Just paths)
      "checked-display2" -> do
        unless (authCount == 18) (fail "invalid checked-display admission")
        admission <- CheckedDisplayAdmission <$> digestField <*> digestField <*> decodeWord64
          <*> nonempty <*> decodeWord64 <*> decodeWord64 <*> digestField <*> decodeWord64
          <*> bounded 65536 string <*> bounded 64 (array 2 >> (,) <$> nonempty <*> digestField)
          <*> bounded 4096 nonempty
          <*> bounded 4096 (array 2 >> ((,) <$> nonempty <*> bounded 65536 nonempty))
          <*> nonempty
          <*> plannedDeclaration
          <*> completedValues <*> valueInterfaces
        validateInterfaces (displayInjectedModules admission) (displayValueInterfaces admission)
        validateValues (displayValueImports admission) (displayCompletedValues admission)
        unique "display injected modules" (displayInjectedModules admission)
        unique "display value import owners" (map fst (displayValueImports admission))
        unique "display value import names" (concatMap snd (displayValueImports admission))
        unless (displayPresentation admission `elem` ["rendered","opaque"]
            && all ((`elem` displayInjectedModules admission) . fst) (displayValueImports admission))
          (fail "invalid display presentation or imported owner")
        paths <- includePaths
        pure (Nothing,Nothing,Just admission,Nothing,Just paths)
      _ -> fail "unsupported exact compile purpose"
    includePaths = bounded 4096 $ do
      path <- absolute
      when (T.length (T.pack path) > 65536) (fail "checked search path exceeds bound")
      pure path
    valueInterfaces = bounded 4096 $ do
      array 4
      ExactIfaceArtifact <$> nonempty <*> nonempty <*> absolute <*> digestField <*> pure []
    validateInterfaces injected values = do
      unique "checked value interface owners" (map exactModule values)
      let canonicalValue value = case parseSessionModule (exactModule value) of
            Just valueOwner -> smKind valueOwner == ValMod && sessionModuleString valueOwner == exactModule value
            Nothing -> False
      unless (all ((== "main") . exactUnit) values && all canonicalValue values
          && length values == length injected && all (`elem` injected) (map exactModule values))
        (fail "checked value bytes differ from injected owner inventory")
    completedValues = bounded 4096 $ do
      array 5
      CompletedValueImport <$> nonempty <*> nonempty <*> absolute <*> digestField
        <*> bounded 65536 (array 2 >> (,) <$> nonempty <*> decodeWord64)
    validateValues imports values = unless
      (map (\value -> (completedValueModule value,map fst (completedValueBinders value))) values == imports
        && all ((== "main") . completedValueUnit) values)
      (fail "completed value identities differ from exact prefix imports")
    plannedDeclaration = do
      token <- peekTokenType
      if token == TypeNull then decodeNull >> pure Nothing else do
        array 3
        unit <- nonempty
        ownerModule <- nonempty
        fingerprint <- nonempty
        unless (unit == "main" && "Tidepool.Session.Lib.G" `isPrefixOf` ownerModule
            && length fingerprint == 32 && all isHexDigit fingerprint)
          (fail "invalid completed original declaration identity")
        pure (Just ((unit,ownerModule),fingerprint))
    signature = decodeCheckedSignature

identity :: Decoder s SymbolIdentity
identity = do
  array 5
  unit <- decodeString
  name <- decodeString
  namespace <- decodeString
  occurrence <- decodeString
  token <- peekTokenType
  parent <- if token == TypeNull then decodeNull >> pure Nothing else Just <$> decodeString
  pure (SymbolIdentity unit name namespace occurrence parent)

owner :: Decoder s (String, String)
owner = array 2 >> (,) <$> nonempty <*> nonempty

array :: Int -> Decoder s ()
array count = decodeListLen >>= \actual -> unless (actual == count) (fail "invalid exact scope row")

bounded :: Int -> Decoder s a -> Decoder s [a]
bounded limit item = do
  count <- decodeListLen
  when (count > limit) (fail "exact scope inventory exceeds bound")
  replicateM count item

unique :: Ord a => String -> [a] -> Decoder s ()
unique label values = unless (Set.size (Set.fromList values) == length values) (fail ("duplicate " ++ label))

string :: Decoder s String
string = T.unpack <$> decodeString

nonempty :: Decoder s String
nonempty = do
  value <- string
  unless (not (null value)) (fail "empty exact owner")
  pure value

absolute :: Decoder s FilePath
absolute = do
  value <- string
  unless (isAbsolute value) (fail "relative exact artifact path")
  pure value

digestField :: Decoder s String
digestField = do
  value <- string
  unless (length value == 64 && all isHexDigit value) (fail "invalid exact digest")
  pure value

digest :: BS.ByteString -> String
digest = concatMap (\byte -> let value = showHex byte "" in replicate (2 - length value) '0' ++ value)
  . BS.unpack . SHA.hash
