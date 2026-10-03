{-# LANGUAGE OverloadedStrings #-}

-- Original source recipes supplement GHC splice execution. Their identities
-- never authorize a lexical import or replace a retained native interface.
module Tidepool.ExecutionSource
  ( ExecutionSourceGraph(..), ExecutionSourceIdentity(..), ExecutionSourceOwner(..)
  , ExecutionSourceRef(..), decodeExecutionSources, decodeExecutionSourceGraph, decodeExecutionSourceReferences, executionIdentityKey
  , ExecutionSourceNode(..), ExecutionSourceFailure(..), executionSourceClosure, executionSourceOriginalNode, executionSourceOriginalClosure
  , ExecutionSourceRecipe(..), issueExecutionSourceRecipe
  , executionSourceProspectiveReferences
  , executionSourceGraphBytesLimit
  ) where

import Codec.CBOR.Decoding
import Codec.CBOR.Read (deserialiseFromBytes)
import Codec.CBOR.Encoding
import Codec.CBOR.Write (toLazyByteString)
import Control.Monad (replicateM, unless, when, forM)
import Control.Exception (Exception)
import qualified Crypto.Hash.SHA256 as SHA
import qualified Data.ByteString as BS
import qualified Data.ByteString.Lazy as BL
import Data.List (nub, sort)
import qualified Data.Text as T
import qualified Data.Text.Encoding as TE
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import Numeric (showHex)
import System.FilePath (isAbsolute)
import Tidepool.DependencyEvidence
import Tidepool.Session (parseSessionModule)

-- Match the certified graph inventory bound in tidepool-toolchain. Metadata
-- and inline candidate manifests retain their separate four MiB envelopes.
executionSourceGraphBytesLimit :: Int
executionSourceGraphBytesLimit = 64 * 1024 * 1024

data ExecutionSourceIdentity = ExecutionSourceIdentity
  { executionUnit :: String, executionModule :: String
  , executionVersion :: String, executionIfaceSha256 :: String
  , executionNativeSha256 :: String
  } deriving (Eq, Ord, Show)

executionIdentityKey :: ExecutionSourceIdentity -> (String, String)
executionIdentityKey original = (executionUnit original, executionModule original)

data ExecutionSourceOwner = ExecutionSourceOwner
  { executionOwnerIdentity :: ExecutionSourceIdentity
  , executionOwnerFresh :: Bool
  , executionOwnerOriginalGraph :: Maybe String
  } deriving (Eq, Show)

data ExecutionSourceRef = ExecutionSourceRef
  { executionRefIdentity :: ExecutionSourceIdentity
  , executionRefGraph :: String
  } deriving (Eq, Show)

data ExecutionSourceGraph = ExecutionSourceGraph
  { executionGraphSha256 :: String
  , executionGraphBytes :: BS.ByteString
  , executionGraphProducer :: String
  , executionGraphSemantic :: Maybe String
  , executionGraphIncludes :: [FilePath]
  , executionGeneratedOrigin :: (FilePath, String)
  , executionGraphEvidence :: DependencyEvidence
  , executionGraphOwners :: [ExecutionSourceOwner]
  , executionGraphExactImports :: [((String, String), [(String, String)])]
  , executionGraphPackages :: [(String, String, FilePath, String)]
  }

-- A successful compiler transaction supplies the same original recipe fields
-- consumed by the wire decoder. Native/interface identities are the already
-- retained products; this record grants no lexical imports.
data ExecutionSourceRecipe = ExecutionSourceRecipe
  { recipeProducer :: String
  , recipeSemantic :: Maybe String
  , recipeIncludes :: [FilePath]
  , recipeGeneratedOrigin :: (FilePath, String)
  , recipeEvidence :: DependencyEvidence
  , recipeOwners :: [ExecutionSourceOwner]
  , recipeExactImports :: [((String, String), [(String, String)])]
  , recipePackages :: [(String, String, FilePath, String)]
  }

-- Local recipes share the existing encoding and admission decoder. Unsupported
-- or oversized source evidence supplies no optional execution capability.
issueExecutionSourceRecipe :: ExecutionSourceRecipe
  -> Either ExecutionSourceFailure (Maybe ExecutionSourceGraph)
issueExecutionSourceRecipe recipe
  | not (dependencyCacheSafe evidence && dependencySelectionComplete evidence)
      || not boundedRecipe = Right Nothing
  | not completeOwners || not generatedMatches || not validOriginals || not validExactImports =
      Left (ExecutionSourceIncomplete ("", "transaction execution recipe"))
  | BS.length bytes > limit = Right Nothing
  | otherwise = case deserialiseFromBytes (decodeGraph (digest bytes) bytes) (BL.fromStrict bytes) of
      Right (remaining, graph) | BL.null remaining -> Right (Just graph)
      _ -> Left (ExecutionSourceIncomplete ("", "transaction execution recipe"))
  where
    evidence = recipeEvidence recipe
    limit = 4 * 1024 * 1024
    ownerKeys = map (executionIdentityKey . executionOwnerIdentity) (recipeOwners recipe)
    ownerSet = Set.fromList ownerKeys
    sourceOwners = Set.fromList [(dependencyModuleUnit node,dependencyModuleName node)
      | node <- dependencyModules evidence, not (dependencyModuleBoot node)]
    completeOwners = ownerKeys == Set.toAscList ownerSet
      && all (\node -> dependencyModuleBoot node || dependencyModuleProduct node /= ProductReady
        || (dependencyModuleUnit node,dependencyModuleName node) `Set.member` ownerSet) (dependencyModules evidence)
    validOriginals = recipeProducer recipe /= replicate 64 '0'
      && all ((/= Just (replicate 64 '0')) . executionOwnerOriginalGraph) (recipeOwners recipe)
    validExactImports = all (\(key, imports) -> key `Set.member` sourceOwners
      && imports == Set.toAscList (Set.fromList imports)) (recipeExactImports recipe)
    generatedMatches = case [row | row <- dependencySources evidence
        , dependencySourcePath row == "@generated-source"] of
      [row] -> dependencySourceSha256 row == digest (TE.encodeUtf8 (T.pack (snd (recipeGeneratedOrigin recipe))))
      _ -> False
    boundedRecipe = all (<= 4096)
      [length (recipeIncludes recipe), length (recipeOwners recipe)
      , length (dependencySources evidence), length (dependencyModules evidence)
      , length (dependencyPackages evidence), length (recipePackages recipe)
      , length (recipeExactImports recipe)]
      && length (dependencyResolutions evidence) <= 65536
      && all ((<= 4096) . length . dependencyResolutionCandidates) (dependencyResolutions evidence)
      && all ((<= 4096) . length . dependencyModuleImports) (dependencyModules evidence)
      && sum (map (length . dependencyModuleImports) (dependencyModules evidence)) <= 65536
      && all ((<= 4096) . length . snd) (recipeExactImports recipe)
      && sum (map (length . snd) (recipeExactImports recipe)) <= 65536
    bytes = BL.toStrict (BL.take (fromIntegral limit + 1) (toLazyByteString encoding))
    text' = encodeString . T.pack
    list item values = encodeListLen (fromIntegral (length values)) <> foldMap item values
    optional' item = maybe encodeNull item
    ownerKey (unit, name) = encodeListLen 2 <> text' unit <> text' name
    identity' original = text' (executionUnit original) <> text' (executionModule original)
      <> text' (executionVersion original) <> text' (executionIfaceSha256 original)
      <> text' (executionNativeSha256 original)
    productKind = \case
      ProductReady -> "ready"
      ProductBoot -> "boot"
      ProductInterfaceOnly -> "interface_only"
      ProductMissingInterface -> "missing_interface"
      ProductProjectionRejected -> "projection_rejected"
    encoding = encodeListLen 11 <> text' "TPEXECUTIONSOURCE" <> encodeWord 1
      <> text' "tidepool-ghc-pipeline-v1" <> text' (recipeProducer recipe)
      <> optional' text' (recipeSemantic recipe) <> list text' (recipeIncludes recipe)
      <> (let (path, source) = recipeGeneratedOrigin recipe
          in encodeListLen 2 <> text' path <> text' source)
      <> encodeListLen 6 <> encodeBool (dependencyCacheSafe evidence)
      <> encodeBool (dependencySelectionComplete evidence)
      <> list (\source -> encodeListLen 2 <> text' (dependencySourcePath source)
          <> text' (dependencySourceSha256 source)) (dependencySources evidence)
      <> list (\row -> encodeListLen 5 <> text' (renderDependencyQualifier (dependencyResolutionQualifier row))
          <> text' (dependencyResolutionModule row) <> encodeBool (dependencyResolutionBoot row)
          <> optional' text' (dependencyResolutionSelected row)
          <> list text' (dependencyResolutionCandidates row)) (dependencyResolutions evidence)
      <> list (\node -> encodeListLen 6 <> text' (dependencyModuleUnit node)
          <> text' (dependencyModuleName node) <> encodeBool (dependencyModuleBoot node)
          <> text' (dependencyModuleSource node)
          <> list (\edge -> encodeListLen 4 <> text' (renderDependencyQualifier (dependencyImportQualifier edge))
              <> text' (dependencyImportName edge) <> encodeBool (dependencyImportBoot edge)
              <> optional' text' (dependencyImportSelected edge)) (dependencyModuleImports node)
          <> text' (productKind (dependencyModuleProduct node))) (dependencyModules evidence)
      <> list text' (dependencyPackages evidence)
      <> list (\owner' -> encodeListLen 7 <> identity' (executionOwnerIdentity owner')
          <> encodeBool (executionOwnerFresh owner')
          <> optional' text' (executionOwnerOriginalGraph owner')) (recipeOwners recipe)
      <> list (\(key, imports) -> encodeListLen 3 <> text' (fst key) <> text' (snd key)
          <> list ownerKey imports) (recipeExactImports recipe)
      <> list (\(unit, name, path, sha) -> encodeListLen 4 <> text' unit <> text' name
          <> text' path <> text' sha) (recipePackages recipe)

instance Eq ExecutionSourceGraph where
  left == right = executionGraphBytes left == executionGraphBytes right

instance Show ExecutionSourceGraph where
  show graph = "ExecutionSourceGraph " ++ show (executionGraphSha256 graph)

data ExecutionSourceNode = ExecutionSourceNode
  { executionNodeIdentity :: ExecutionSourceIdentity
  , executionNodeGraph :: ExecutionSourceGraph
  , executionNodeModule :: DependencyModule
  , executionNodeSourceSha256 :: String
  , executionNodeRequirements :: [(String, String)]
  }

data ExecutionSourceFailure
  = ExecutionSourceMissing (String, String)
  | ExecutionSourceConflicting (String, String)
  | ExecutionSourceUnsupported (String, String)
  | ExecutionSourceIncomplete (String, String)
  | ExecutionSourceChanged (String, String)
  | ExecutionSourceResolutionChanged (String, String)
  | ExecutionSourceImportResolutionChanged (String, String)
      [(DependencyQualifier,String,Bool,Maybe FilePath)] [(DependencyQualifier,String,Bool,Maybe FilePath)]
  | ExecutionSourceSearchChanged (String, String) [FilePath]
  | ExecutionSourcePackageChanged (String, String)
  | ExecutionSourceLinkableMissing (String, String)
  | ExecutionSourceUnavailable (String, String)
  deriving (Eq, Show)

instance Exception ExecutionSourceFailure

-- A retained owner can use only its original graph. Consumer-issued source
-- rows for a cached owner cannot backfill an absent execution capability.
executionSourceClosure
  :: [ExecutionSourceGraph] -> [ExecutionSourceRef] -> [ExecutionSourceIdentity]
  -> [(String, String)] -> Either ExecutionSourceFailure [ExecutionSourceNode]
executionSourceClosure graphs references nativeOwners roots = do
  let referenceMap = Map.fromList [(executionIdentityKey (executionRefIdentity ref),ref) | ref <- references]
      nativeMap = Map.fromList [(executionIdentityKey original,original) | original <- nativeOwners]
  pending <- mapM (\key -> case Map.lookup key referenceMap of
    Nothing -> Left (ExecutionSourceMissing key)
    Just reference -> Right (executionRefIdentity reference,executionRefGraph reference)) roots
  walkExecutionSources graphs (CurrentRecipeClosure nativeMap referenceMap) pending

-- Advertised provenance is validated separately from current capability
-- availability. Missing/corrupt original recipes cannot become optional misses.
executionSourceOriginalClosure :: [ExecutionSourceGraph] -> [ExecutionSourceRef]
  -> Either ExecutionSourceFailure [ExecutionSourceNode]
executionSourceOriginalClosure graphs references = walkExecutionSources graphs OriginalRecipeClosure
  [(executionRefIdentity reference,executionRefGraph reference) | reference <- references]

-- A new local product need not have an executable source recipe. In this mode
-- an explicitly absent capability is optional; promised original graphs still
-- use the same strict traversal and cannot be silently discarded.
executionSourceProspectiveReferences :: [ExecutionSourceGraph] -> [ExecutionSourceRef]
  -> [ExecutionSourceRef] -> Either ExecutionSourceFailure [ExecutionSourceRef]
executionSourceProspectiveReferences graphs inherited prospective = do
  _ <- executionSourceOriginalClosure graphs inherited
  concat <$> forM prospective (\reference ->
    case walkExecutionSources graphs ProspectiveRecipeClosure
        [(executionRefIdentity reference,executionRefGraph reference)] of
      Left (ExecutionSourceUnavailable _) -> Right []
      Left refusal -> Left refusal
      Right _ -> Right [reference])

data ExecutionRecipeSelection
  = OriginalRecipeClosure
  | ProspectiveRecipeClosure
  | CurrentRecipeClosure (Map.Map (String,String) ExecutionSourceIdentity)
      (Map.Map (String,String) ExecutionSourceRef)

walkExecutionSources :: [ExecutionSourceGraph] -> ExecutionRecipeSelection
  -> [(ExecutionSourceIdentity,String)] -> Either ExecutionSourceFailure [ExecutionSourceNode]
walkExecutionSources graphs selection pending = Map.elems . fst <$>
  visit Map.empty Set.empty Set.empty pending
  where
    currentIdentity original = case selection of
      OriginalRecipeClosure -> Right ()
      ProspectiveRecipeClosure -> Right ()
      CurrentRecipeClosure native _ -> unless
        (Map.lookup (executionIdentityKey original) native == Just original)
        (Left (ExecutionSourceUnavailable (executionIdentityKey original)))
    visit selected completed _ [] = Right (selected,completed)
    visit selected completed active ((original,sha) : rest) = do
      currentIdentity original
      node <- case executionSourceOriginalNodeWith prospective graphs original sha of
        Left (ExecutionSourceUnsupported key) | prospective -> Left (ExecutionSourceUnavailable key)
        result -> result
      let key = executionIdentityKey original
          recipe = (original,executionGraphSha256 (executionNodeGraph node))
      when (recipe `Set.member` active) (Left (ExecutionSourceConflicting key))
      if recipe `Set.member` completed then visit selected completed active rest else do
        case Map.lookup key selected of
          Just existing -> do
            -- Authenticate both local recipes and their dependencies before
            -- sharing a source-load node. Transaction-wide graph identity is
            -- not the identity of an individual original source recipe.
            equivalent <- sameLocalRecipe existing node
            unless equivalent (Left (ExecutionSourceConflicting key))
          Nothing -> pure ()
        required <- mapM (dependency node) (executionNodeRequirements node)
        (dependencies,completedDependencies) <- visit selected completed (Set.insert recipe active) required
        -- A different graph context can reach this owner while validating
        -- children. It must agree before the parent's node is shared too.
        case Map.lookup key dependencies of
          Just existing -> do
            equivalent <- sameLocalRecipe existing node
            unless equivalent (Left (ExecutionSourceConflicting key))
          Nothing -> pure ()
        forMSelected node dependencies
        let shared = Map.insertWith (\_ existing -> existing) key node dependencies
        visit shared (Set.insert recipe completedDependencies) active rest
    sameLocalRecipe left right = do
      leftObligations <- obligations left
      rightObligations <- obligations right
      pure (executionNodeIdentity left == executionNodeIdentity right
        && executionNodeSourceSha256 left == executionNodeSourceSha256 right
        && moduleRecipe left == moduleRecipe right
        && leftObligations == rightObligations
        && resolutionRecipe left == resolutionRecipe right)
    moduleRecipe node = let source = executionNodeModule node in
      (dependencyModuleUnit source,dependencyModuleName source,dependencyModuleBoot source,
       dependencyModuleSource source,dependencyModuleProduct source,
       sort (map importRecipe (dependencyModuleImports source)))
    importRecipe edge = (dependencyImportQualifier edge,dependencyImportName edge,
      dependencyImportBoot edge,dependencyImportSelected edge)
    obligations node = mapM (\key -> do
      owner' <- suppliedOwner node key
      pure (executionOwnerIdentity owner',executionOwnerFresh owner',executionOwnerOriginalGraph owner'))
      (executionNodeRequirements node)
    resolutionRecipe node = sort
      [(dependencyResolutionQualifier row,dependencyResolutionModule row,
        dependencyResolutionBoot row,dependencyResolutionSelected row,dependencyResolutionCandidates row)
      | row <- dependencyResolutions (executionGraphEvidence (executionNodeGraph node))
      , any (\edge -> dependencyImportQualifier edge == dependencyResolutionQualifier row
          && dependencyImportName edge == dependencyResolutionModule row
          && dependencyImportBoot edge == dependencyResolutionBoot row)
          (dependencyModuleImports (executionNodeModule node))]
    suppliedOwner node key = case [owner' | owner' <- executionGraphOwners (executionNodeGraph node)
        , executionIdentityKey (executionOwnerIdentity owner') == key] of
      [owner'] -> Right owner'
      [] | prospective -> Left (ExecutionSourceUnavailable key)
      _ -> Left (ExecutionSourceIncomplete key)
    dependency node key = do
      supplied <- suppliedOwner node key
      let original = executionOwnerIdentity supplied
      currentIdentity original
      case (executionOwnerFresh supplied,executionOwnerOriginalGraph supplied) of
        (True,Nothing) -> Right (original,executionGraphSha256 (executionNodeGraph node))
        (False,Just expected) -> case selection of
          OriginalRecipeClosure -> Right (original,expected)
          ProspectiveRecipeClosure -> do
            -- A retained digest promises a complete original closure. Only
            -- newly encountered local capability gaps may be optional.
            _ <- executionSourceOriginalClosure graphs [ExecutionSourceRef original expected]
            Right (original,expected)
          CurrentRecipeClosure _ references -> do
            reference <- maybe (Left (ExecutionSourceUnavailable key)) Right (Map.lookup key references)
            unless (executionRefIdentity reference == original && executionRefGraph reference == expected)
              (Left (ExecutionSourceUnavailable key))
            Right (original,expected)
        (False,Nothing) | prospective -> Left (ExecutionSourceUnavailable key)
        (False,Nothing) -> Left (ExecutionSourceMissing key)
        (True,Just _) -> Left (ExecutionSourceConflicting key)
    prospective = case selection of
      ProspectiveRecipeClosure -> True
      _ -> False
    forMSelected node selected = mapM_ (\edge -> case dependencyImportSelected edge of
        Nothing -> Right ()
        Just path -> case Map.lookup
            (dependencyModuleUnit (executionNodeModule node),dependencyImportName edge) selected of
          Just child | dependencyModuleSource (executionNodeModule child) == path -> Right ()
          _ -> Left (ExecutionSourceConflicting (executionIdentityKey (executionNodeIdentity node))))
      (dependencyModuleImports (executionNodeModule node))

-- Validate the original recipe itself independently of which dependency
-- capabilities survived current native candidate admission. This does not
-- authorize execution; executionSourceClosure checks the selected closure.
executionSourceOriginalNode :: [ExecutionSourceGraph] -> ExecutionSourceIdentity
  -> String -> Either ExecutionSourceFailure ExecutionSourceNode
executionSourceOriginalNode = executionSourceOriginalNodeWith False

executionSourceOriginalNodeWith :: Bool -> [ExecutionSourceGraph] -> ExecutionSourceIdentity
  -> String -> Either ExecutionSourceFailure ExecutionSourceNode
executionSourceOriginalNodeWith prospective graphs = originalNode prospective Set.empty
  where
    graphMap = Map.fromList [(executionGraphSha256 graph,graph) | graph <- graphs]
    originalNode allowUnavailable seen original sha
      | sha `Set.member` seen = Left (ExecutionSourceConflicting key)
      | otherwise = do
          graph <- maybe (Left (ExecutionSourceMissing key)) Right (Map.lookup sha graphMap)
          owner' <- one key [owner' | owner' <- executionGraphOwners graph
            , executionOwnerIdentity owner' == original]
          case (executionOwnerFresh owner', executionOwnerOriginalGraph owner') of
            (False, Just retained) -> originalNode False (Set.insert sha seen) original retained
            (False, Nothing) | allowUnavailable -> Left (ExecutionSourceUnavailable key)
            (False, Nothing) -> Left (ExecutionSourceMissing key)
            (True, Just _) -> Left (ExecutionSourceConflicting key)
            (True, Nothing) -> do
              node <- one key [node | node <- dependencyModules (executionGraphEvidence graph)
                , (dependencyModuleUnit node,dependencyModuleName node) == key
                , not (dependencyModuleBoot node)]
              unless (parseSessionModule (snd key) == Nothing
                  && dependencyModuleProduct node == ProductReady
                  && isAbsolute (dependencyModuleSource node)
                  && not (any dependencyImportBoot (dependencyModuleImports node)))
                (Left (ExecutionSourceUnsupported key))
              source <- one key [source | source <- dependencySources (executionGraphEvidence graph)
                , dependencySourcePath source == dependencyModuleSource node]
              let selected = [(dependencyModuleUnit node,dependencyImportName edge)
                    | edge <- dependencyModuleImports node, dependencyImportSelected edge /= Nothing]
                  exact = concat [imports | (ownerKey,imports) <- executionGraphExactImports graph, ownerKey == key]
                  requirements = Set.toAscList (Set.fromList (selected ++ exact))
              pure (ExecutionSourceNode original graph node (dependencySourceSha256 source) requirements)
      where key = executionIdentityKey original
    one key values = case values of
      [value] -> Right value
      _ -> Left (ExecutionSourceIncomplete key)

decodeExecutionSources :: Decoder s ([ExecutionSourceGraph], [ExecutionSourceRef])
decodeExecutionSources = do
  array 2
  graphs <- bounded 4096 $ do
    array 2
    sha <- digestField
    bytes <- decodeBytes
    either fail pure (decodeExecutionSourceGraph sha bytes)
  references <- decodeExecutionSourceReferences
  unique "original execution graphs" (map executionGraphSha256 graphs)
  pure (graphs, references)

-- Both candidate parcels and exact-scope graph files consume the same bytes.
decodeExecutionSourceGraph :: String -> BS.ByteString -> Either String ExecutionSourceGraph
decodeExecutionSourceGraph sha bytes
  | digest bytes /= sha = Left "original execution graph digest differs"
  | otherwise = case deserialiseFromBytes (decodeGraph sha bytes) (BL.fromStrict bytes) of
      Left reason -> Left (show reason)
      Right (remaining, graph)
        | BL.null remaining -> Right graph
        | otherwise -> Left "original execution graph has trailing bytes"

decodeExecutionSourceReferences :: Decoder s [ExecutionSourceRef]
decodeExecutionSourceReferences = do
  references <- bounded 4096 (array 6 >> ExecutionSourceRef <$> identity <*> digestField)
  unique "original execution references" (map (executionIdentityKey . executionRefIdentity) references)
  pure references

decodeGraph :: String -> BS.ByteString -> Decoder s ExecutionSourceGraph
decodeGraph sha bytes = do
  array 11
  magic <- text
  version <- decodeWord
  profile <- text
  unless (magic == "TPEXECUTIONSOURCE" && version == 1
      && profile == "tidepool-ghc-pipeline-v1") (fail "unsupported original execution graph")
  producer <- digestField
  semantic <- optional digestField
  includes <- bounded 4096 absolute
  array 2
  origin <- absolute
  source <- text
  array 6
  safe <- decodeBool
  complete <- decodeBool
  unless (safe && complete) (fail "incomplete original execution evidence")
  sources <- bounded 4096 (array 2 >> DependencySource <$> sourcePath <*> digestField)
  resolutions <- bounded 65536 $ do
    array 5
    DependencyResolution <$> qualifier <*> nonempty <*> decodeBool <*> optional sourcePath
      <*> bounded 4096 sourcePath
  modules <- withEdgeBudget 4096 65536 $ \remaining -> do
    array 6
    unit <- nonempty
    name <- nonempty
    boot <- decodeBool
    path <- sourcePath
    imports <- bounded (min 4096 remaining) $ do
      array 4
      DependencyImport <$> qualifier <*> nonempty <*> decodeBool <*> optional sourcePath
    productKind <- text >>= \case
      "ready" -> pure ProductReady
      "boot" -> pure ProductBoot
      "interface_only" -> pure ProductInterfaceOnly
      "missing_interface" -> pure ProductMissingInterface
      "projection_rejected" -> pure ProductProjectionRejected
      _ -> fail "unsupported original execution product kind"
    pure (DependencyModule unit name boot path imports productKind, length imports)
  packages <- bounded 4096 nonempty
  owners <- bounded 4096 $ do
    array 7
    original <- identity
    fresh <- decodeBool
    retained <- optional digestField
    when (fresh && retained /= Nothing) (fail "fresh original execution owner has a retained graph")
    pure (ExecutionSourceOwner original fresh retained)
  exactImports <- withEdgeBudget 4096 65536 $ \remaining -> do
    array 3
    unit <- nonempty
    name <- nonempty
    imports <- bounded (min 4096 remaining) owner
    unique "original execution exact imports" imports
    pure (((unit, name), imports), length imports)
  roots <- bounded 4096 (array 4 >> (,,,) <$> nonempty <*> nonempty <*> absolute <*> digestField)
  unique "original execution source paths" (map dependencySourcePath sources)
  unique "original execution module owners"
    [(dependencyModuleUnit node,dependencyModuleName node,dependencyModuleBoot node) | node <- modules]
  unique "original execution native owners" (map (executionIdentityKey . executionOwnerIdentity) owners)
  unique "original execution exact owners" (map fst exactImports)
  unique "original execution package owners" [(unit,name) | (unit,name,_,_) <- roots]
  unless (not (null sources) && not (null modules) && not (null owners))
    (fail "empty original execution source proof")
  let evidence = DependencyEvidence safe complete sources resolutions packages modules
  pure (ExecutionSourceGraph sha bytes producer semantic includes (origin,source) evidence owners exactImports roots)

withEdgeBudget :: Int -> Int -> (Int -> Decoder s (a, Int)) -> Decoder s [a]
withEdgeBudget limit budget item = do
  count <- decodeListLen
  when (count > limit) (fail "original execution inventory exceeds bound")
  let consume 0 _ = pure []
      consume n remaining = do
        (value, used) <- item remaining
        (value :) <$> consume (n - 1) (remaining - used)
  consume count budget

identity :: Decoder s ExecutionSourceIdentity
identity = ExecutionSourceIdentity <$> nonempty <*> nonempty <*> digestField <*> digestField <*> digestField

owner :: Decoder s (String, String)
owner = array 2 >> (,) <$> nonempty <*> nonempty

optional :: Decoder s a -> Decoder s (Maybe a)
optional item = peekTokenType >>= \case
  TypeNull -> decodeNull >> pure Nothing
  _ -> Just <$> item

array :: Int -> Decoder s ()
array expected = decodeListLen >>= \actual -> unless (actual == expected)
  (fail "invalid original execution row")

bounded :: Int -> Decoder s a -> Decoder s [a]
bounded limit item = do
  count <- decodeListLen
  when (count > limit) (fail "original execution inventory exceeds bound")
  replicateM count item

unique :: Eq a => String -> [a] -> Decoder s ()
unique label values = unless (length (nub values) == length values) (fail ("duplicate " ++ label))

text :: Decoder s String
text = do
  value <- decodeString
  when (T.length value > 4 * 1024 * 1024) (fail "original execution text exceeds bound")
  pure (T.unpack value)

nonempty :: Decoder s String
nonempty = do
  value <- text
  unless (not (null value) && length value <= 65536) (fail "invalid original execution owner")
  pure value

absolute :: Decoder s FilePath
absolute = do
  value <- nonempty
  unless (isAbsolute value) (fail "relative original execution path")
  pure value

sourcePath :: Decoder s FilePath
sourcePath = do
  value <- nonempty
  unless (isAbsolute value || value == "@generated-source") (fail "relative original execution source")
  pure value

qualifier :: Decoder s DependencyQualifier
qualifier = do
  value <- nonempty
  maybe (fail "invalid original execution import qualifier") pure (parseDependencyQualifier value)

digestField :: Decoder s String
digestField = do
  value <- text
  unless (length value == 64 && all (`elem` ("0123456789abcdef" :: String)) value)
    (fail "invalid original execution digest")
  pure value

digest :: BS.ByteString -> String
digest = concatMap (\byte -> let value = showHex byte "" in replicate (2 - length value) '0' ++ value)
  . BS.unpack . SHA.hash
