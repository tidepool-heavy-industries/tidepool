{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE OverloadedStrings #-}
-- | Editable views over retained evidence. The planner is independent of Jev;
-- a lens and a judge are ordinary functions the companion can replace.
module Persona.Attention
  ( Memory(..), Collection, collection, memories, Vantage(..), knownBy
  , Resolution(..), Perspective(..), Presentation(..), Node(..), View(..)
  , Limits(..), Decision(..), Batch(..), Outcome(..), StopReason(..)
  , Problem(..), Prepared, Inspection(..), inspect, rendered, snapshot
  , prepareWith, evidence, evidenceSize, combineViews, continuity, connections
  ) where

import Prelude
import Control.Monad (foldM)
import Data.List (find)
import Data.Text (Text)
import qualified Data.Text as T
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import GHC.Generics (Generic)
import Tidepool.Aeson (ToJSON(..), Value(..), toText)
import Tidepool.Inspection.Display (Display(..), genericDisplayTree)
import Tidepool.Inspection.Tree (DisplayTree(..))

-- | availableAt is an authored knowledge boundary, not file modification time.
-- The original text is retained even when a view offers a shorter presentation.
data Memory = Memory
  { memoryId :: Text, sourcePath :: Text, original :: Text
  , tags :: [Text], related :: [Text], availableAt :: Maybe Integer
  } deriving (Eq, Show, Generic)
instance ToJSON Memory
instance Display Memory where displayTree = genericDisplayTree

newtype Collection = Collection [Memory] deriving (Eq, Show)
instance Display Collection where displayTree = displayTree . memories
memories :: Collection -> [Memory]
memories (Collection values) = values

data Problem
  = DuplicateMemory Text | MissingMemory Text | DuplicateNode Text
  | MissingNode Text | Cycle [Text] | InvalidNode Text | InvalidLimits
  deriving (Eq, Show, Generic)
instance ToJSON Problem
instance Display Problem where displayTree = genericDisplayTree

collection :: [Memory] -> Either Problem Collection
collection values = do
  _ <- foldM (\seen m -> if memoryId m `Set.member` seen
        then Left (DuplicateMemory (memoryId m))
        else Right (Set.insert (memoryId m) seen)) Set.empty values
  mapM_ (\key -> if key `elem` map memoryId values then Right () else Left (MissingMemory key))
    (concatMap related values)
  pure (Collection values)

data Vantage = Present | KnownBy Integer deriving (Eq, Show, Generic)
instance ToJSON Vantage
instance Display Vantage where displayTree = genericDisplayTree

-- | Unknown dates are not evidence that a source existed at a historical boundary.
-- Removed relationships cannot leak future memory identifiers into a lens.
knownBy :: Vantage -> Collection -> (Collection, [Text])
knownBy Present values = (values, [])
knownBy (KnownBy cutoff) (Collection values) =
  let allowed m = maybe False (<= cutoff) (availableAt m)
      retained = filter allowed values
      keys = Set.fromList (map memoryId retained)
  in (Collection [m {related = filter (`Set.member` keys) (related m)} | m <- retained],
      map memoryId (filter (not . allowed) values))

data Resolution = Outline | Detail | Original deriving (Eq, Show, Generic)
instance ToJSON Resolution
instance Display Resolution where displayTree = genericDisplayTree
data Perspective = Evidence | OpenQuestion | Connection | Interpretation Text
  deriving (Eq, Show, Generic)
instance ToJSON Perspective
instance Display Perspective where displayTree = genericDisplayTree
data Presentation = Presentation
  { presentationId :: Text, whenUseful :: Text, resolution :: Resolution
  , perspective :: Perspective, text :: Text, sources :: [Text]
  } deriving (Eq, Show, Generic)
instance ToJSON Presentation
instance Display Presentation where displayTree = genericDisplayTree
data Node = Node
  { nodeId :: Text, question :: Text, fallback :: Presentation
  , alternatives :: [Presentation], children :: [Text]
  } deriving (Eq, Show, Generic)
instance ToJSON Node
instance Display Node where displayTree = genericDisplayTree
data View = View { roots :: [Text], nodes :: [Node] } deriving (Eq, Show, Generic)
instance ToJSON View
instance Display View where displayTree = genericDisplayTree

-- | Shared node IDs mean the same question over the same presentations.
-- Conflicting definitions are refused rather than silently taking one lens.
combineViews :: [View] -> Either Problem View
combineViews views = do
  merged <- foldM add [] (concatMap nodes views)
  pure (View (concatMap roots views) merged)
  where
    add retained n = case find ((== nodeId n) . nodeId) retained of
      Nothing -> Right (retained <> [n])
      Just previous | previous == n -> Right retained
      Just _ -> Left (DuplicateNode (nodeId n))

-- | Operational budgets chosen by the caller. Evidence is counted in Unicode
-- characters, including repeated occasion text per batch; it is not a token or
-- provider-request-size estimate. Zero permits a deterministic fallback view.
data Limits = Limits { judgmentPasses :: Int, evidenceCharacters :: Integer }
  deriving (Eq, Show, Generic)
instance ToJSON Limits
instance Display Limits where displayTree = genericDisplayTree
data Decision = PresentAs Presentation | Expand | Omit | Uncertain Text
  deriving (Eq, Show, Generic)
instance ToJSON Decision
instance Display Decision where displayTree = genericDisplayTree
data Batch = Batch { decisions :: [(Text, Decision)], observation :: Value }
  deriving (Eq, Show, Generic)
instance ToJSON Batch
instance Display Batch where
  displayTree batch = Constructor "JudgmentBatch"
    [("decisions", displayTree (decisions batch)), ("observation", receiptTree (observation batch))]
data StopReason = PassLimit | EvidenceLimit | Doubt Text | InvalidDecision
  deriving (Eq, Show, Generic)
instance ToJSON StopReason
instance Display StopReason where displayTree = genericDisplayTree
data Outcome
  = Selected Text Presentation | Expanded Text Presentation | Omitted Text
  | Fallback Text Presentation StopReason
  deriving (Eq, Show, Generic)
instance ToJSON Outcome
instance Display Outcome where displayTree = genericDisplayTree
data Inspection = Inspection
  { retainedMemories :: [Memory], retainedView :: View, occasion :: Text
  , limits :: Limits, outcomes :: [Outcome], notVisited :: [Text]
  , batches :: [Batch], passesUsed :: Int, evidenceUsed :: Integer
  , contextText :: Text
  } deriving (Eq, Show, Generic)
instance ToJSON Inspection
instance Display Inspection where displayTree = genericDisplayTree
newtype Prepared = Prepared Inspection deriving (Eq, Show)
inspect :: Prepared -> Inspection
inspect (Prepared value) = value
rendered :: Prepared -> Text
rendered = contextText . inspect
snapshot :: Prepared -> Value
snapshot = toJSON . inspect
instance Display Prepared where
  displayTree = displayTree . inspect

-- Value has no structural Display instance in this pinned interface. Keep the
-- retained Jev wire evidence expandable inside the persona's own inspection.
receiptTree :: Value -> DisplayTree
receiptTree value = case value of
  Null -> TextLeaf "null"
  Bool b -> displayTree b
  Number n -> TextLeaf (T.pack (show n))
  String s -> displayTree s
  Array items -> Sequence "[" "]" (map receiptTree items)
  Object fields -> Constructor "Object" [(toText key, receiptTree item) | (key, item) <- Map.toList fields]

validate :: Collection -> View -> Either Problem (Map.Map Text Node)
validate values view = do
  table <- foldM add Map.empty (nodes view)
  mapM_ (require table) (roots view <> concatMap children (nodes view))
  mapM_ check (nodes view)
  _ <- foldM (visit table []) Set.empty (Map.keys table)
  pure table
  where
    add table n
      | Map.member (nodeId n) table = Left (DuplicateNode (nodeId n))
      | otherwise = Right (Map.insert (nodeId n) n table)
    require table key = if Map.member key table then Right () else Left (MissingNode key)
    check n = do
      let options = fallback n : alternatives n
          keys = map presentationId options
      if T.null (nodeId n) || T.null (question n) || any T.null keys
        || Set.size (Set.fromList keys) /= length keys
        then Left (InvalidNode (nodeId n)) else Right ()
      mapM_ (\key -> if key `elem` map memoryId (memories values)
        then Right () else Left (MissingMemory key)) (concatMap sources options)
    visit table path done key
      | key `elem` path = Left (Cycle (reverse (key:path)))
      | key `Set.member` done = Right done
      | otherwise = do
          n <- maybe (Left (MissingNode key)) Right (Map.lookup key table)
          visited <- foldM (visit table (key:path)) done (children n)
          pure (Set.insert key visited)

-- | This is the deciding evidence sent to the semantic interpreter. Retain the
-- entire view separately, including branches which were never judged.
evidence :: Node -> Text
evidence n = T.intercalate "\n"
  (["View: " <> nodeId n, question n]
   <> [T.intercalate " | " [presentationId p, whenUseful p, T.pack (show (resolution p)),
         T.pack (show (perspective p)), "sources=" <> T.intercalate "," (sources p), text p]
      | p <- fallback n : alternatives n]
   <> ["Expandable branches: " <> T.intercalate "," (children n)])
evidenceSize :: Text -> [Node] -> Integer
evidenceSize intent candidates = toInteger (T.length intent)
  + sum (map (toInteger . T.length . evidence) candidates)

-- | The judge can be Jev, a deterministic rule or an authored human dialogue.
-- Only reached nodes are judged. A shared node is processed once; to ask a
-- different question about the same memory, give that view its own node ID.
prepareWith :: Monad m
  => (Text -> [Node] -> m Batch) -> Limits -> Text -> Collection -> View
  -> m (Either Problem Prepared)
prepareWith judge budget intent values view
  | judgmentPasses budget < 0 || evidenceCharacters budget < 0 = pure (Left InvalidLimits)
  | otherwise = case validate values view of
      Left problem -> pure (Left problem)
      Right table -> Right <$> walk table Set.empty (roots view) [] [] 0 0
  where
    walk table seen pending results receipts passes used = do
      let fresh = unique [key | key <- pending, key `Set.notMember` seen]
          frontier = [n | key <- fresh, Just n <- [Map.lookup key table]]
          seen' = Set.union seen (Set.fromList fresh)
          remaining = evidenceCharacters budget - used
          (admitted, overBudget) = fit remaining frontier
          finish more logs count volume = pure $ Prepared $ Inspection
            (memories values) view intent budget more
            [nodeId n | n <- nodes view, nodeId n `Set.notMember` seen']
            logs count volume (render more)
      if null frontier then finish results receipts passes used
      else if passes >= judgmentPasses budget then
        finish (results <> map (conservative PassLimit) frontier) receipts passes used
      else if null admitted then
        finish (results <> map (conservative EvidenceLimit) frontier) receipts passes used
      else do
        receipt <- judge intent admitted
        let pairs = decisions receipt
            keys = map fst pairs
            complete = Set.fromList keys == Set.fromList (map nodeId admitted)
              && length keys == length admitted
            resolve n
              | n `elem` overBudget = conservative EvidenceLimit n
              | not complete = conservative InvalidDecision n
              | otherwise = case lookup (nodeId n) pairs of
                  Just (PresentAs p) | p `elem` (fallback n : alternatives n) -> Selected (nodeId n) p
                  Just Expand | not (null (children n)) -> Expanded (nodeId n) (fallback n)
                  Just Omit -> Omitted (nodeId n)
                  Just (Uncertain why) -> conservative (Doubt why) n
                  _ -> conservative InvalidDecision n
            selected = map resolve frontier
            next = concat [children n | Expanded key _ <- selected, Just n <- [Map.lookup key table]]
        walk table seen' next (results <> selected) (receipts <> [receipt])
          (passes + 1) (used + evidenceSize intent admitted)
    -- Skip an oversized node without starving smaller siblings. No evidence
    -- prefix is cut off and passed to the judge as though it were complete.
    fit remaining = go (remaining - toInteger (T.length intent)) [] []
      where
        go _ yes no [] = (reverse yes, reverse no)
        go space yes no (n:ns)
          | cost <= space = go (space - cost) (n:yes) no ns
          | otherwise = go space yes (n:no) ns
          where cost = toInteger (T.length (evidence n))
    conservative reason n = Fallback (nodeId n) (fallback n) reason
    unique = reverse . snd . foldl (\(seen, acc) key ->
      if key `Set.member` seen then (seen, acc) else (Set.insert key seen, key:acc)) (Set.empty, [])

render :: [Outcome] -> Text
render results = T.intercalate "\n\n" $
  ["# Prepared memory view\nSaved orientation selected by authored Haskell. This is contextual material, not a new human utterance."]
  <> concatMap section results
  <> ["Omitted and unreached views, original evidence and judgments remain in the retained preparation. Writing memory does not refresh this view."]
  where
    section (Omitted _) = []
    section (Selected key p) = [present key p]
    section (Expanded key p) = [present key p]
    section (Fallback key p reason) = [present key p <> "\n[Unresolved selection: " <> T.pack (show reason) <> "]"]
    present key p = "## " <> key <> "\n[" <> T.pack (show (resolution p)) <> "; "
      <> T.pack (show (perspective p)) <> "; sources: " <> T.intercalate ", " (sources p)
      <> "]\n" <> text p

-- | Two small starting lenses, deliberately ordinary editable functions.
continuity :: Collection -> View
continuity values = View (map (memoryKey . memoryId) items) (map memoryNode items)
  where items = memories values

connections :: Collection -> View
connections values = View (map (connectionKey . memoryId) linked)
  (map connectionNode linked <> map memoryNode items)
  where
    items = memories values
    linked = filter (not . null . related) items
    connectionNode m = Node (connectionKey (memoryId m))
      "Would juxtaposing these related memories help this occasion? Expand to inspect the originals, or keep the open question."
      (Presentation "question" "A relationship is worth carrying as an open question" Outline Connection
        ("What connects " <> memoryId m <> " with " <> T.intercalate ", " (related m) <> "?")
        (memoryId m : related m)) [] (map memoryKey (memoryId m : related m))

memoryKey :: Text -> Text
memoryKey key = "memory/" <> key
connectionKey :: Text -> Text
connectionKey key = "connection/" <> key
memoryNode :: Memory -> Node
memoryNode m = Node (memoryKey (memoryId m))
  ("Which view of this memory helps the occasion? Tags: " <> T.intercalate ", " (tags m))
  (Presentation "original" "Its full original wording matters, or it is unclear what can be omitted" Original Evidence (original m) [memoryId m])
  [Presentation "outline" "A source pointer and brief original excerpt suffice" Outline Evidence
    (sourcePath m <> "\n" <> excerpt (original m)) [memoryId m]] []
  where
    excerpt body = case find (\line -> not (T.null (T.strip line)) && not ("#" `T.isPrefixOf` line)) (T.lines body) of
      Nothing -> "[empty source]"
      Just line -> T.take 240 line <> "\n[Outline excerpt; original retained]"
