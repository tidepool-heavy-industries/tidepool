{-# LANGUAGE OverloadedStrings #-}

module Tidepool.ExactScope
  ( ExactScope(..), ExactProduct(..), ExactOriginalGroup(..), ExactCompilation(..)
  , CheckedCellAdmission(..), CheckedItemAdmission(..), CheckedDisplayAdmission(..)
  , PlannedCellAdmission(..), PlannedCellSlot(..)
  , readExactScope, revalidateExactScope, scopeValueInterfaces
  , writeExactCompilation
  , extendExactExecutionSources, extendExactExecutionSourcesWithinBudget
  , scopeExecutionNativeOwners
  ) where

import Codec.CBOR.Decoding
import Codec.CBOR.Read (deserialiseFromBytes)
import qualified Codec.CBOR.Encoding as E
import Codec.CBOR.Write (toStrictByteString)
import Control.Exception (IOException, try, throwIO)
import Control.Monad (foldM, forM, forM_, replicateM, unless, when)
import qualified Crypto.Hash.SHA256 as SHA
import qualified Data.ByteString as BS
import qualified Data.ByteString.Lazy as BL
import Data.Char (isHexDigit)
import Data.List (nub, isPrefixOf)
import qualified Data.Text as T
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import GHC.Driver.Env (HscEnv)
import Data.Word (Word64)
import Numeric (showHex)
import System.Directory (getFileSize, createDirectory, createDirectoryIfMissing, makeAbsolute)
import System.FilePath (isAbsolute, takeDirectory, (</>))
import System.IO (IOMode(ReadMode), withBinaryFile)
import System.IO.Error (isAlreadyExistsError)
import Tidepool.ExactHydration (ExactIfaceArtifact(..))
import Tidepool.Session (SessionModule(..), SessionModuleKind(..), parseSessionModule, sessionModuleString)
import Tidepool.CheckedPrefixImports (CompletedValueImport(..))
import Tidepool.CheckedCell (CheckedSignature(..), CheckedSignatureName(..))
import Tidepool.ExecutionSchema (SymbolIdentity(..))
import Tidepool.ExecutionSource
  ( ExecutionSourceGraph(..), ExecutionSourceIdentity(..), ExecutionSourceOwner(..)
  , ExecutionSourceRef(..), ExecutionSourceNode(..), decodeExecutionSourceGraph, decodeExecutionSourceReferences
  , ExecutionSourceFailure(..), executionIdentityKey, executionSourceClosure, executionSourceOriginalNode
  , executionSourceOriginalClosure )
import Tidepool.PackageWitness
  ( PackageImportEvidence(..), readPackageImports, validatePackageImportRoot )
import Tidepool.DependencyEvidence
  ( DependencyEvidence(..), DependencySource(..), renderDependencyEvidence
  , revalidateDependencyEvidence )

data ExactScope = ExactScope
  { scopeManifestPath :: FilePath
  , scopeRequestSha256 :: String
  , scopeProducerSha256 :: String
  , scopeSemanticSha256 :: String
  , scopeInterfaces :: [(ExactIfaceArtifact, FilePath, String)]
  , scopeLexical :: [((String, String), [(String, String)])]
  , scopeProducts :: [ExactProduct]
  , scopeExecutionGraphs :: [ExecutionSourceGraph]
  , scopeExecutionOwners :: [ExecutionSourceRef]
  , scopeCheckedCell :: Maybe CheckedCellAdmission
  , scopeCheckedItem :: Maybe CheckedItemAdmission
  , scopeCheckedDisplay :: Maybe CheckedDisplayAdmission
  , scopeIncludePaths :: Maybe [FilePath]
  } deriving (Eq, Show)

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

data CheckedCellAdmission = CheckedCellAdmission
  { checkedAdmissionDigest :: String
  , checkedCellSha256 :: String
  , checkedTemplateSha256 :: String
  , checkedTurnTemplates :: [(String, String)]
  , checkedInjectedModules :: [String]
  , checkedReservedModules :: [String]
  , checkedValueInterfaces :: [ExactIfaceArtifact]
  , checkedPlannedCell :: Maybe PlannedCellAdmission
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

data CheckedItemAdmission = CheckedItemAdmission
  { itemAdmissionDigest :: String
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
  , originalGlobals :: [(SymbolIdentity, Bool)]
  } deriving (Eq, Show)

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
      && sum (map (BS.length . executionGraphBytes) graphs) <= 4 * 1024 * 1024
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

data ExactCompilation = ExactCompilation
  { compilationScope :: ExactScope
  , compilationTransaction :: Word64
  , compilationSource :: FilePath
  , compilationImports :: [((String, String, Bool), [(String, String, Bool, String)])]
  } deriving (Eq, Show)

scopeValueInterfaces :: ExactScope -> [ExactIfaceArtifact]
scopeValueInterfaces scope =
  maybe [] checkedValueInterfaces (scopeCheckedCell scope)
    ++ maybe [] itemValueInterfaces (scopeCheckedItem scope)
    ++ maybe [] displayValueInterfaces (scopeCheckedDisplay scope)

-- Scope v6 separates the bounded metadata envelope from the independently
-- bounded original graph bytes. The request hash seals each path and digest.
readExactScope :: FilePath -> IO (Either String ExactScope)
readExactScope path = do
  captured <- try (do
    unless (isAbsolute path) (fail "exact scope path must be absolute")
    bytes <- readBoundedFile path (4 * 1024 * 1024)
    (scope, descriptors) <- case deserialiseFromBytes decodeScope (BL.fromStrict bytes) of
      Left failure -> fail (show failure)
      Right (remaining, result)
        | BL.null remaining -> pure result
        | otherwise -> fail "exact scope has trailing bytes"
    sizes <- forM descriptors $ \(_, graphPath) -> do
      unless (takeDirectory graphPath == takeDirectory path)
        (fail "original execution graph is outside its request directory")
      getFileSize graphPath
    when (sum sizes > 4 * 1024 * 1024)
      (fail "original execution graphs exceed four MiB")
    graphs <- forM (zip descriptors sizes) $ \((sha, graphPath), size) -> do
      graphBytes <- readBoundedFile graphPath (fromIntegral size)
      unless (toInteger (BS.length graphBytes) == size)
        (fail "original execution graph size changed")
      either fail pure (decodeExecutionSourceGraph sha graphBytes)
    validateExecutionSources scope graphs
    pure scope { scopeManifestPath = path, scopeRequestSha256 = digest bytes
      , scopeExecutionGraphs = graphs }) :: IO (Either IOException ExactScope)
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

-- Recheck the entire producer-owned closure in the consuming transaction;
-- no source file is a substitute for an admitted original interface.
revalidateExactScope :: HscEnv -> ExactScope -> IO (Either String ())
revalidateExactScope env scope = do
  result <- try (do
    bytes <- readBoundedFile (scopeManifestPath scope) (4 * 1024 * 1024)
    unless (digest bytes == scopeRequestSha256 scope) (fail "exact scope request changed")
    mapM_ checkInterface (scopeInterfaces scope)
    mapM_ checkProduct (scopeProducts scope)
    mapM_ checkValue (scopeValueInterfaces scope))
    :: IO (Either IOException ())
  pure $ either (Left . show) Right result
  where
    checkInterface (iface, packages, packagesSha) = do
      roots <- readPackageImports packages packagesSha iface
      selected <- either fail pure roots
      mapM_ (\root -> validatePackageImportRoot env root >>= either fail pure) (packageInterfaces selected)
    checkValue value = do
      bytes <- BS.readFile (exactPath value)
      unless (digest bytes == exactSha256 value) (fail "checked value interface changed")
    checkProduct originalProduct = do
      bytes <- BS.readFile (originalProductPath originalProduct)
      unless (digest bytes == originalProductSha256 originalProduct)
        (fail "exact original product changed")

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
  let parent = takeDirectory path </> ".exact-compilations"
  createDirectoryIfMissing True parent
  directory <- reserveCompilationDirectory parent transaction
  let snapshot = directory </> "source.hs"
      encodeArray values = E.encodeListLen (fromIntegral (length values)) <> mconcat values
      text = E.encodeString . T.pack
      importRow (qualifier, name, boot, unit) = encodeArray
        [text qualifier, text name, E.encodeBool boot, text unit]
      moduleRow ((unit, name, boot), edges) = encodeArray
        [text unit, text name, E.encodeBool boot, encodeArray (map importRow edges)]
      receipt = encodeArray
        [text "TPEXACTCOMPILE", text "1", text (scopeRequestSha256 scope)
        , text (scopeSemanticSha256 scope), text path, text (digest bytes)
        , text snapshot, text (renderDependencyEvidence evidence)
        , encodeArray (map moduleRow imports)]
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

decodeScope :: Decoder s (ExactScope, [(String, FilePath)])
decodeScope = do
  count <- decodeListLen
  magic <- string
  version <- string
  unless (magic == "TPEXACTSCOPE"
      && ((version == "2" && count == 7) || (version == "4" && count == 8)
        || (version == "6" && count == 9)))
    (fail "unsupported exact scope")
  semantic <- digestField
  producer <- digestField
  interfaces <- bounded 4096 $ do
    array 7
    unit <- nonempty
    name <- nonempty
    path <- absolute
    sha <- digestField
    requirements <- bounded 4096 owner
    packages <- absolute
    packageSha <- digestField
    unique "exact requirements" requirements
    pure (ExactIfaceArtifact unit name path sha requirements, packages, packageSha)
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
      && all (\(iface, _, _) -> all (`elem` keys) (exactRequirements iface)) interfaces
      && all (\originalProduct -> any (\(iface, _, _) ->
          (exactUnit iface, exactModule iface) == (originalUnit originalProduct, originalModule originalProduct)
          && exactSha256 iface == originalIfaceSha256 originalProduct) interfaces) products)
    (fail "incomplete or conflicting exact owner closure")
  (descriptors, executionOwners) <- if version == "6" then do
    array 2
    graphs <- bounded 4096 (array 2 >> (,) <$> digestField <*> absolute)
    references <- decodeExecutionSourceReferences
    unique "original execution graphs" (map fst graphs)
    pure (graphs, references)
    else pure ([], [])
  nullPurpose <- if version == "6" then (== TypeNull) <$> peekTokenType else pure False
  (checked, checkedItem, checkedDisplay, includes) <- if version == "2" || nullPurpose
    then do
      when nullPurpose decodeNull
      pure (Nothing,Nothing,Nothing,Nothing)
    else do
    authCount <- decodeListLen
    purpose <- string
    case purpose of
      "cell-check2" -> do
        unless (authCount == 9) (fail "invalid cell-check admission")
        admission <- CheckedCellAdmission <$> digestField <*> digestField <*> digestField
          <*> bounded 64 (array 2 >> (,) <$> nonempty <*> digestField)
          <*> bounded 4096 nonempty <*> bounded 4096 nonempty <*> valueInterfaces <*> pure Nothing
        validateInterfaces (checkedInjectedModules admission) (checkedValueInterfaces admission)
        unique "checked injected modules" (checkedInjectedModules admission)
        unique "checked reserved modules" (checkedReservedModules admission)
        paths <- includePaths
        pure (Just admission,Nothing,Nothing,Just paths)
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
        validateInterfaces (checkedInjectedModules admission) (checkedValueInterfaces admission)
        paths <- includePaths
        pure (Just admission,Nothing,Nothing,Just paths)
      "checked-item2" -> do
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
        paths <- includePaths
        pure (Nothing, Just (CheckedItemAdmission admissionDigest receiptDigest index sourceDigest kind binders
          templates injected signatures liftPlan presentation generation prefix valueImports observation planned values valueInputs), Nothing, Just paths)
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
        pure (Nothing,Nothing,Just admission,Just paths)
      _ -> fail "unsupported exact compile purpose"
  pure (ExactScope "" "" producer semantic interfaces lexical products [] executionOwners
    checked checkedItem checkedDisplay includes, descriptors)
  where
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
    signature = do
      array 3
      key <- nonempty
      rendering <- nonempty
      names <- bounded 65536 (array 5 >> (CheckedSignatureName <$> nonempty <*> nonempty <*> nonempty <*> nonempty <*> nonempty))
      unique "checked signature qualifiers" (map signatureQualifier names)
      pure (CheckedSignature key rendering names)

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

unique :: Eq a => String -> [a] -> Decoder s ()
unique label values = unless (length (nub values) == length values) (fail ("duplicate " ++ label))

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
