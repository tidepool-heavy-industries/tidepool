{-# LANGUAGE OverloadedStrings #-}

-- Original source recipes supplement GHC splice execution. Their identities
-- never authorize a lexical import or replace a retained native interface.
module Tidepool.ExecutionSource
  ( ExecutionSourceGraph(..), ExecutionSourceIdentity(..), ExecutionSourceOwner(..)
  , ExecutionSourceRef(..), decodeExecutionSourceGraph, decodeExecutionSourceReferences, executionIdentityKey
  , executionSourceInheritedOwners
  , ExecutionSourceNode(..), ExecutionSourceFailure(..), ExecutionSourceValidationStage(..)
  , ExecutionSourceInterfaceReason(..), executionSourceClosure, executionSourceOriginalNode, executionSourceOriginalClosure
  , ExecutionSourceRecipe(..), issueExecutionSourceRecipe
  , executionSourceProspectiveReferences
  , executionNodeOriginalResolutions
  , decodeExecutionSourceDescriptors, readExecutionSourceGraphs, readExecutionSourceGraphsWith, readExecutionSourceGraphsWithFacts, executionSourceGraphsFit
  , executionSourceGraphBytesLimit
  , WorkerExecutionSource(..), SourceRecipeUnavailable(..), encodeWorkerExecutionSource
  ) where

import Codec.CBOR.Decoding
import Codec.CBOR.Read (deserialiseFromBytes)
import Codec.CBOR.Encoding
import Codec.CBOR.Write (toLazyByteString)
import Control.Monad (replicateM, unless, when, forM, foldM)
import Control.Exception (Exception)
import qualified Crypto.Hash.SHA256 as SHA
import qualified Data.ByteString as BS
import qualified Data.ByteString.Lazy as BL
import Data.List (sort)
import qualified Data.Text as T
import qualified Data.Text.Encoding as TE
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import Numeric (showHex)
import GHC.Iface.Recomp (CompileReason)
import GHC.Utils.Outputable (defaultSDocContext, ppr, renderWithContext)
import System.Directory (getFileSize)
import System.FilePath (isAbsolute)
import System.IO (IOMode(ReadMode), withBinaryFile)
import Tidepool.DependencyEvidence
import Tidepool.Session (isReservedSessionModuleName)
import Tidepool.Timing (emitCount, readTimingEnabled)

-- Match the certified graph inventory bound in tidepool-toolchain. Metadata
-- and candidate manifests retain their separate four MiB envelopes.
executionSourceGraphBytesLimit :: Int
executionSourceGraphBytesLimit = 64 * 1024 * 1024

-- Authored source UTF-8 has the same independent bound as Rust graph admission.
executionSourceSourceBytesLimit :: Int
executionSourceSourceBytesLimit = 32 * 1024 * 1024

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

-- Optional original recipes belong to the complete retained native identity.
-- Combining separately admitted inputs must not choose a reference by spelling
-- or silently replace another original recipe for that owner.
executionSourceInheritedOwners :: [ExecutionSourceRef] -> [ExecutionSourceIdentity]
  -> Either ExecutionSourceFailure [ExecutionSourceOwner]
executionSourceInheritedOwners references originals = do
  prior <- foldM insertReference Map.empty references
  Map.elems <$> foldM (retain prior) Map.empty originals
  where
    insertReference selected reference =
      let key = executionIdentityKey (executionRefIdentity reference)
      in case Map.lookup key selected of
        Nothing -> Right (Map.insert key reference selected)
        Just previous | previous == reference -> Right selected
        _ -> Left (ExecutionSourceConflicting key)
    retain prior selected original = do
      let key = executionIdentityKey original
      graph <- case Map.lookup key prior of
        Nothing -> Right Nothing
        Just reference | executionRefIdentity reference == original ->
          Right (Just (executionRefGraph reference))
        _ -> Left (ExecutionSourceConflicting key)
      let retainedOwner = ExecutionSourceOwner original False graph
      case Map.lookup key selected of
        Nothing -> Right (Map.insert key retainedOwner selected)
        Just previous | previous == retainedOwner -> Right selected
        _ -> Left (ExecutionSourceConflicting key)

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
-- Exact compilation issues one immutable recipe for both later worker passes
-- and frontend admission. Ordinary compilation has no worker recipe consumer.
data WorkerExecutionSource
  = OrdinaryExecutionSource
  | ExactExecutionSourceUnavailable SourceRecipeUnavailable
  | ExactExecutionSourceAvailable ExecutionSourceGraph

data SourceRecipeUnavailable
  = NoFreshOriginals
  | IncompleteSourceEvidence
  | UnsupportedSourceRecipe
  | UnavailableSourceRoot

encodeWorkerExecutionSource :: WorkerExecutionSource -> Encoding
encodeWorkerExecutionSource OrdinaryExecutionSource =
  encodeListLen 1 <> encodeString "ordinary"
encodeWorkerExecutionSource (ExactExecutionSourceAvailable graph) =
  encodeListLen 2 <> encodeString "exact-available"
    <> encodeString (T.pack (executionGraphSha256 graph))
encodeWorkerExecutionSource (ExactExecutionSourceUnavailable reason) =
  encodeListLen 2 <> encodeString "exact-unavailable" <> encodeString (case reason of
    NoFreshOriginals -> "no-fresh-originals"
    IncompleteSourceEvidence -> "incomplete-source-evidence"
    UnsupportedSourceRecipe -> "unsupported-source-recipe"
    UnavailableSourceRoot -> "unavailable-source-root")

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
    limit = executionSourceGraphBytesLimit
    generatedSource = TE.encodeUtf8 (T.pack (snd (recipeGeneratedOrigin recipe)))
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
      [row] -> dependencySourceSha256 row == digest generatedSource
      _ -> False
    boundedRecipe = BS.length generatedSource <= executionSourceSourceBytesLimit
      && all (<= 4096)
      [length (recipeIncludes recipe), length (recipeOwners recipe)
      , length (dependencySources evidence), length (dependencyModules evidence)
      , length (dependencyPackages evidence), length (recipePackages recipe)
      , length (recipeExactImports recipe)]
      && length (dependencyResolutions evidence) <= 65536
      && all ((<= 4096) . length . dependencyResolutionCandidates) (dependencyResolutions evidence)
      && sum (map (toInteger . length . dependencyResolutionCandidates)
        (dependencyResolutions evidence)) <= 65536
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
  -- Graph IDs refer to the request's authenticated inventory. Identical local
  -- recipes still retain every context's original negative search witnesses.
  , executionNodeOriginalGraphs :: Set.Set String
  }

executionNodeOriginalResolutions
  :: [ExecutionSourceGraph] -> ExecutionSourceNode
  -> Either ExecutionSourceFailure [DependencyResolution]
executionNodeOriginalResolutions graphs = \node -> do
  graphMap <- inventory
  let imports = Set.fromList [(dependencyImportQualifier edge,dependencyImportName edge,dependencyImportBoot edge)
        | edge <- dependencyModuleImports (executionNodeModule node)]
      context sha = do
        graph <- maybe (Left (ExecutionSourceMissing (executionIdentityKey (executionNodeIdentity node)))) Right
          (Map.lookup sha graphMap)
        pure [row | row <- dependencyResolutions (executionGraphEvidence graph)
          , (dependencyResolutionQualifier row,dependencyResolutionModule row,dependencyResolutionBoot row)
              `Set.member` imports]
  concat <$> mapM context (Set.toAscList (executionNodeOriginalGraphs node))
  where
    inventory = foldM insertGraph Map.empty graphs
    insertGraph selected graph = case Map.lookup (executionGraphSha256 graph) selected of
      Nothing -> Right (Map.insert (executionGraphSha256 graph) graph selected)
      Just previous | executionGraphBytes previous == executionGraphBytes graph -> Right selected
      _ -> Left (ExecutionSourceConflicting ("",executionGraphSha256 graph))

newtype ExecutionSourceInterfaceReason = ExecutionSourceInterfaceReason CompileReason
  deriving (Eq)

instance Show ExecutionSourceInterfaceReason where
  showsPrec precedence (ExecutionSourceInterfaceReason reason) =
    showsPrec precedence (renderWithContext defaultSDocContext (ppr reason))

-- Diagnostic facts retain the validation owner and stage; they grant no authority.
data ExecutionSourceValidationStage
  = CurrentSourceSelectionIncomplete
  | OriginalSourceBytesChanged FilePath String String
  | FreshSourceSummaryChanged (String, String) (Maybe FilePath) String (Maybe FilePath) String
  | OriginalSourceObservationChanged FilePath (Maybe FilePath) String String String String
  | OriginalInterfaceRecompileRequired ExecutionSourceInterfaceReason
  | BeforeMergeSourceBytesChanged FilePath String String
  deriving (Eq, Show)

data ExecutionSourceFailure
  = ExecutionSourceMissing (String, String)
  | ExecutionSourceConflicting (String, String)
  | ExecutionSourceUnsupported (String, String)
  | ExecutionSourceIncomplete (String, String)
  | ExecutionSourceChanged (String, String)
  | ExecutionSourceChangedDuring (String, String) ExecutionSourceValidationStage
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
        -- Pending and completed pairs are disjoint. Reserve the next context
        -- before descending or growing any per-node witness union.
        when (Set.size completed + Set.size active >= executionSourceContextLinksLimit)
          (Left (ExecutionSourceIncomplete key))
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
        shared <- case Map.lookup key dependencies of
          Nothing -> Right (Map.insert key node dependencies)
          Just existing -> do
            let contexts = executionNodeOriginalGraphs existing
                offered = executionNodeOriginalGraphs node
                newContexts = length [context | context <- Set.toList offered
                  , context `Set.notMember` contexts]
            when (Set.size contexts + newContexts > executionSourceGraphsLimit)
              (Left (ExecutionSourceIncomplete key))
            Right (Map.insert key (existing {executionNodeOriginalGraphs=Set.union contexts offered}) dependencies)
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
      -- Freshness and graph pointers locate a dependency in its issuing
      -- transaction. The exact original is its semantic obligation; recursive
      -- traversal authenticates each pointer and compares the child recipes.
      pure (executionOwnerIdentity owner'))
      (executionNodeRequirements node)
    resolutionRecipe node = sort
      [(dependencyResolutionQualifier row,dependencyResolutionModule row,
        dependencyResolutionBoot row,dependencyResolutionSelected row)
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
              unless (not (isReservedSessionModuleName (snd key))
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
              pure (ExecutionSourceNode original graph node (dependencySourceSha256 source) requirements
                (Set.singleton (executionGraphSha256 graph)))
      where key = executionIdentityKey original
    one key values = case values of
      [value] -> Right value
      _ -> Left (ExecutionSourceIncomplete key)

executionSourceGraphsLimit :: Int
executionSourceGraphsLimit = 4096

-- One request can select the same immutable owner through several original
-- graphs. Bound those links separately from the authenticated graph inventory.
executionSourceContextLinksLimit :: Int
executionSourceContextLinksLimit = 65536

-- These retained graph bounds are independent of the metadata envelope.
executionSourceGraphsFit :: [ExecutionSourceGraph] -> Bool
executionSourceGraphsFit graphs = length graphs <= executionSourceGraphsLimit
  && sum (map (toInteger . BS.length . executionGraphBytes) graphs)
    <= toInteger executionSourceGraphBytesLimit

decodeExecutionSourceDescriptors :: Decoder s [(String, FilePath)]
decodeExecutionSourceDescriptors = do
  descriptors <- bounded executionSourceGraphsLimit (array 2 >> (,) <$> digestField <*> absolute)
  unique "original execution graphs" (map fst descriptors)
  pure descriptors

-- Exact scopes carry file paths issued by the Rust immutable artifact owner;
-- the request retains its issuing artifact custody. Candidate offers transport
-- those same owned paths. File placement never establishes graph authority.
-- Capture one graph at a time after checking the complete retained byte budget.
-- The file paths transport bytes; only their authenticated graph digests and
-- original references can establish product compatibility.
readExecutionSourceGraphs :: FilePath -> [ExecutionSourceGraph]
  -> [(String, FilePath)] -> IO [ExecutionSourceGraph]
readExecutionSourceGraphs manifest known descriptors = do
  unless (isAbsolute manifest && length descriptors <= executionSourceGraphsLimit
      && Set.size (Set.fromList (map fst descriptors)) == length descriptors)
    (fail "invalid original execution graph descriptor inventory")
  let captured = Map.fromList [(executionGraphSha256 graph,graph) | graph <- known]
  sizes <- forM descriptors $ \(sha, path) -> do
    unless (isAbsolute path)
      (fail "original execution graph path must be absolute")
    size <- getFileSize path
    when (size > toInteger executionSourceGraphBytesLimit)
      (fail "original execution graphs exceed 64 MiB")
    case Map.lookup sha captured of
      Just graph -> unless (size == toInteger (BS.length (executionGraphBytes graph)))
        (fail "original execution graph size changed")
      Nothing -> pure ()
    pure size
  let new = [(sha,size) | ((sha,_),size) <- zip descriptors sizes, Map.notMember sha captured]
      retainedBytes = sum (map (toInteger . BS.length . executionGraphBytes) (Map.elems captured))
  when (Map.size captured + length new > executionSourceGraphsLimit
      || retainedBytes + sum (map snd new) > toInteger executionSourceGraphBytesLimit)
    (fail "original execution graphs exceed their retained byte bound")
  timing <- readTimingEnabled
  forM (zip descriptors sizes) $ \((sha, path), size) -> do
    bytes <- withBinaryFile path ReadMode $ \handle -> BS.hGet handle (fromInteger size + 1)
    unless (toInteger (BS.length bytes) == size)
      (fail "original execution graph size changed")
    graph <- case Map.lookup sha captured of
      Just graph -> do
        unless (digest bytes == sha) (fail "original execution graph digest differs")
        pure graph
      Nothing -> either fail pure (decodeExecutionSourceGraph sha bytes)
    emitCount timing ("hash_bytes.execution_graph." ++ sha) (fromIntegral (BS.length bytes))
    pure graph

-- Admission reads through the request capture owner. Retained bytes and decoded
-- graphs share custody; per-graph and aggregate graph bounds remain independent
-- of the encompassing request budget.
readExecutionSourceGraphsWith :: (FilePath -> Int -> IO BS.ByteString)
  -> FilePath -> [(String, FilePath)] -> IO [ExecutionSourceGraph]
readExecutionSourceGraphsWith readInput manifest descriptors = do
  readExecutionSourceGraphsWithFacts (\sha path -> do
    payload <- readInput path executionSourceGraphBytesLimit
    either fail pure (decodeExecutionSourceGraph sha payload)) manifest descriptors

readExecutionSourceGraphsWithFacts :: (String -> FilePath -> IO ExecutionSourceGraph)
  -> FilePath -> [(String,FilePath)] -> IO [ExecutionSourceGraph]
readExecutionSourceGraphsWithFacts readFacts manifest descriptors = do
  unless (isAbsolute manifest && length descriptors <= executionSourceGraphsLimit
      && Set.size (Set.fromList (map fst descriptors)) == length descriptors
      && all (isAbsolute . snd) descriptors)
    (fail "invalid original execution graph descriptor inventory")
  graphs <- mapM (uncurry readFacts) descriptors
  unless (and [executionGraphSha256 graph == sha | ((sha,_),graph) <- zip descriptors graphs])
    (fail "original execution graph facts differ from receiving selection")
  when (sum (map (toInteger . BS.length . executionGraphBytes) graphs) > toInteger executionSourceGraphBytesLimit)
    (fail "original execution graphs exceed their retained byte bound")
  pure graphs

-- Both candidate parcels and exact-scope graph files consume the same bytes.
decodeExecutionSourceGraph :: String -> BS.ByteString -> Either String ExecutionSourceGraph
decodeExecutionSourceGraph sha bytes
  | BS.length bytes > executionSourceGraphBytesLimit = Left "original execution graphs exceed 64 MiB"
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
  source <- sourceText
  array 6
  safe <- decodeBool
  complete <- decodeBool
  unless (safe && complete) (fail "incomplete original execution evidence")
  sources <- bounded 4096 (array 2 >> DependencySource <$> sourcePath <*> digestField)
  resolutions <- withEdgeBudget 65536 65536 $ \remaining -> do
    array 5
    q <- qualifier
    name <- nonempty
    boot <- decodeBool
    selected <- optional sourcePath
    candidates <- bounded (min 4096 remaining) sourcePath
    pure (DependencyResolution q name boot selected candidates, length candidates)
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

unique :: Ord a => String -> [a] -> Decoder s ()
unique label values = unless (Set.size (Set.fromList values) == length values) (fail ("duplicate " ++ label))

sourceText :: Decoder s String
sourceText = do
  value <- decodeString
  when (BS.length (TE.encodeUtf8 value) > executionSourceSourceBytesLimit)
    (fail "original execution source exceeds its 32 MiB UTF-8 bound")
  pure (T.unpack value)

text :: Decoder s String
text = T.unpack <$> decodedText

decodedText :: Decoder s T.Text
decodedText = do
  value <- decodeString
  when (T.length value > 4 * 1024 * 1024) (fail "original execution text exceeds bound")
  pure value

nonempty :: Decoder s String
nonempty = do
  value <- decodedText
  unless (not (T.null value) && T.length value <= 65536) (fail "invalid original execution owner")
  pure (T.unpack value)

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
