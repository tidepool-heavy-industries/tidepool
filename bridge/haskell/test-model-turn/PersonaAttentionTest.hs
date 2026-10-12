{-# LANGUAGE DataKinds #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}
module PersonaAttentionTest (personaAttentionTests) where

import Prelude
import Control.Monad.Freer (Eff, Member, interpret, run)
import Control.Monad.Freer.State (State, get, modify, runState)
import Data.Text (Text)
import qualified Data.Text as T
import qualified Data.Map.Strict as Map
import Test.Tasty.HUnit (assertBool, assertEqual)
import Test.Tasty.QuickCheck (testProperty, NonNegative(..), (===), conjoin)
import Tidepool.Test.Runner (TestTree, testGroup, testCase)
import Tidepool.Aeson (Value(..), object, (.=))
import Tidepool.Effects.Core (FsRead(..), FsWrite(..), Jev(..), JevCallError(..))
import Tidepool.Records (FsError(..))
import Tidepool.Inspection.Display (Display(..))
import qualified Persona.Attention as A
import qualified Persona.Collection as C
import qualified Persona.Context as Context

must :: Show e => Either e a -> a
must = either (error . show) id

source :: A.Collection
source = must $ A.collection
  [ A.Memory "map" "notes/map.md" "A hand drawn map of a place we imagined." ["maps"] ["door"] (Just 10)
  , A.Memory "door" "notes/door.md" "The human corrected my assumption: the door opens inward." [] ["map"] (Just 20)
  ]

offer :: Text -> Text -> A.Presentation
offer key body = A.Presentation key "Useful when this angle helps" A.Detail A.Evidence body ["map"]
node :: Text -> [Text] -> A.Node
node key children = A.Node key "How should this memory be presented?"
  (offer "outline" ("Outline of " <> key)) [offer "full" ("Full wording of " <> key)] children
diamond :: A.View
diamond = A.View ["left", "right"]
  [node "left" ["shared"], node "right" ["shared"], node "shared" ["unread"], node "unread" []]
generous :: A.Limits
generous = A.Limits 10 100000

plan :: (A.Node -> A.Decision) -> A.Limits -> A.View
  -> (Either A.Problem A.Prepared, [[Text]])
plan policy budget view = run $ runState [] $
  A.prepareWith observe budget "Explore maps together" source view
  where
    observe _ ns = do
      modify (<> [map A.nodeId ns])
      pure (A.Batch [(A.nodeId n, policy n) | n <- ns] (String "authored deterministic policy"))

personaAttentionTests :: TestTree
personaAttentionTests = testGroup "persona attention"
  [ testCase "batched branching reuses a shared node without visiting an unselected branch" $ do
      let policy n = if A.nodeId n == "shared" then A.PresentAs (head (A.alternatives n)) else A.Expand
          (result, calls) = plan policy generous diamond
          retained = A.inspect (must result)
      assertEqual "frontier batches" [["left", "right"], ["shared"]] calls
      assertEqual "one retained outcome per shared view" 3 (length (A.outcomes retained))
      assertEqual "unselected branch remains inspectable" ["unread"] (A.notVisited retained)
      assertEqual "originals retained once" (A.memories source) (A.retainedMemories retained)
      assertBool "selected detail rendered" ("Full wording of shared" `T.isInfixOf` A.contextText retained)
  , testCase "cycles and missing source references fail before calling a judge" $ do
      let invalidViews = [A.View ["x"] [node "x" ["x"]], A.View ["absent"] [],
            A.View ["x"] [(node "x" []) {A.fallback = (offer "outline" "missing") {A.sources = ["missing"]}}]]
      mapM_ (\v -> do
        let (result, calls) = plan (const A.Expand) generous v
        assertEqual "no external judgment on invalid view" [] calls
        assertBool "invalid graph refused" (case result of Left _ -> True; Right _ -> False)) invalidViews
  , testCase "budget exhaustion preserves a visible fallback and never calls Jev" $ do
      mapM_ (\budget -> do
        let (result, calls) = plan (const A.Omit) budget diamond
            retained = A.inspect (must result)
        assertEqual "no budget no calls" [] calls
        assertEqual "both roots retained" 2 (length (A.outcomes retained))
        assertBool "reason visible" ("Unresolved selection" `T.isInfixOf` A.contextText retained))
        [A.Limits 0 100000, A.Limits 10 0]
  , testCase "oversized evidence does not starve a smaller sibling or truncate it" $ do
      let huge = (node "huge" []) {A.fallback = offer "outline" (T.replicate 10000 "x")}
          small = node "small" []
          budget = A.Limits 1 (A.evidenceSize "Explore maps together" [small])
          (result, calls) = plan (A.PresentAs . A.fallback) budget (A.View ["huge", "small"] [huge, small])
      assertEqual "only complete evidence sent" [["small"]] calls
      assertEqual "exact measured evidence" (A.evidenceCharacters budget) (A.evidenceUsed (A.inspect (must result)))
  , testCase "malformed decisions keep every candidate uncertain" $ do
      let judge _ _ = pure (A.Batch [("left", A.Omit), ("left", A.Omit)] Null)
          result = run (A.prepareWith judge generous "maps" source diamond)
      assertBool "no omission from duplicate/missing decisions"
        (all (\o -> case o of A.Fallback _ _ A.InvalidDecision -> True; _ -> False)
          (A.outcomes (A.inspect (must result))))
  , testCase "inspection and alternate pure projections perform no effects" $ do
      let (result, calls) = plan (A.PresentAs . A.fallback) generous diamond
          prepared = must result
      assertEqual "one batch" [["left", "right"]] calls
      assertEqual "same retained rendering" (A.rendered prepared) (A.contextText (A.inspect prepared))
      assertEqual "inspection retains original batches" 1 (length (A.batches (A.inspect prepared)))
      let (preview, truncated) = displayWith 100000 (A.inspect prepared)
      assertBool "structured inspection exposes fields" ("retainedMemories" `T.isInfixOf` preview && "outcomes" `T.isInfixOf` preview)
      assertBool "complete fixture inspection is neither opaque nor truncated" (not truncated && not ("<opaque>" `T.isInfixOf` preview))
  , testCase "human correction can replace an omission using the same evidence" $ do
      let v = A.continuity source
          (omitted, _) = plan (const A.Omit) generous v
          (corrected, _) = plan (A.PresentAs . A.fallback) generous v
      assertBool "old rendering omitted the correction" (not ("opens inward" `T.isInfixOf` A.rendered (must omitted)))
      assertBool "new rendering restores original words" ("opens inward" `T.isInfixOf` A.rendered (must corrected))
      assertEqual "selection does not rewrite sources" (A.retainedMemories (A.inspect (must omitted)))
        (A.retainedMemories (A.inspect (must corrected)))
  , testCase "historical lens excludes later and undated evidence and relationships" $ do
      let original = must $ A.collection (A.memories source <> [A.Memory "undated" "notes/x.md" "unknown date" [] [] Nothing])
          (past, omitted) = A.knownBy (A.KnownBy 10) original
      assertEqual "source coverage" ["map"] (map A.memoryId (A.memories past))
      assertEqual "explicit exclusions" ["door", "undated"] omitted
      assertEqual "future relationship removed" [[]] (map A.related (A.memories past))
      assertEqual "no future identifiers in connections lens" [] (A.roots (A.connections past))
  , testCase "production Jev refusal retains a whole frontier as uncertainty" $ do
      let (result, calls) = run (runState (0 :: Int) (interpret jevUnavailable
            (Context.prepareView generous "maps" source diamond)))
          retained = A.inspect (must result)
      assertEqual "one batched Jev effect" 1 calls
      assertEqual "roots retained" 2 (length (A.outcomes retained))
      assertBool "unavailability visible" ("JevCircuitOpen" `T.isInfixOf` A.contextText retained)
      assertEqual "no children silently interpreted" ["shared", "unread"] (A.notVisited retained)
  , testCase "default memory lens sends one valid batch and retains full originals on Jev failure" $ do
      let (result, calls) = run (runState (0 :: Int) (interpret jevUnavailable
            (Context.prepareView generous "maps" source (A.continuity source))))
          prepared = must result
          (preview, truncated) = displayWith 100000 prepared
      assertEqual "actual Jev call including dynamic source IDs" 1 calls
      assertBool "human correction survives a failed judgment" ("opens inward" `T.isInfixOf` A.rendered prepared)
      assertBool "wire request is structurally inspectable" ("request" `T.isInfixOf` preview && not truncated && not ("<opaque>" `T.isInfixOf` preview))
  , testCase "optional absent manifest preserves edited files and creates only missing blocks" $ do
      let initial = Map.fromList [("memory/persona.md", Right "I enjoy maps."), ("notes/other.md", Right "untouched")]
          (result, files) = run (runState @Files initial (interpret readHost (interpret writeHost C.load)))
          loaded = must result
      assertEqual "identity preserved" (Just (Right "I enjoy maps.")) (Map.lookup "memory/persona.md" files)
      assertEqual "unrelated state preserved" (Just (Right "untouched")) (Map.lookup "notes/other.md" files)
      assertEqual "three observed blocks" 3 (length (A.memories (C.collection loaded)))
      assertEqual "manifest absence retained" Nothing (C.manifestObservation loaded)
  , testCase "joined preparation loads saved state, judges once and leaves it unchanged" $ do
      let files = Map.fromList [("memory/persona.md", Right "I enjoy maps."),
            ("memory/human.md", Right "The door opens inward."),
            ("memory/curiosities.md", Right "What lives beyond that door?")]
          action :: Eff '[FsWrite, FsRead, Jev, State Int, State Files]
            (Either Context.PreparationError (C.Loaded, A.Prepared))
          action = Context.prepare generous "Explore the imagined place" A.continuity
          ((result, calls), after) = run (runState @Files files (runState (0 :: Int)
            (interpret jevUnavailable (interpret readHost (interpret writeHost action)))))
          (loaded, prepared) = must result
      assertEqual "one semantic pass over loaded memory" 1 calls
      assertEqual "preparation preserves saved sources" files after
      assertEqual "rendering uses the observed collection" (A.memories (C.collection loaded))
        (A.retainedMemories (A.inspect prepared))
      assertBool "saved human correction is present" ("The door opens inward." `T.isInfixOf` A.rendered prepared)
  , testCase "unreadable manifest and source are failures rather than missing memories" $ do
      let files = Map.fromList [("memory/collection.json", Left (FsIo "permission denied"))]
          (result, _) = run (runState @Files files (interpret readHost (interpret writeHost C.load)))
      case result of
        Left (C.Storage "memory/collection.json" (FsIo "permission denied")) -> pure ()
        _ -> fail "unreadable manifest was mistaken for an absent optional manifest"
      let originals = Map.fromList [(path, Right "present") | path <- ["memory/persona.md", "memory/human.md", "memory/curiosities.md"]]
          (extra, _) = run (runState @Files originals (interpret readHost
            (C.loadEntries (C.Manifest [C.Entry "extra" "notes/missing.md" [] [] Nothing]))))
      case extra of
        Left (C.Storage "notes/missing.md" (FsNotFound _)) -> pure ()
        _ -> fail "missing original was converted into empty evidence"
  , testCase "manifest production parser and loader preserve metadata and original words" $ do
      let manifest = must $ C.manifestFromValue (object ["version" .= (1 :: Int), "memories" .=
            [object ["id" .= ("extra" :: Text), "path" .= ("notes/extra.md" :: Text),
              "tags" .= (["map"] :: [Text]), "related" .= (["persona"] :: [Text]), "availableAt" .= (20 :: Int)]]])
          files = Map.fromList [(p, Right p) | p <- ["memory/persona.md", "memory/human.md", "memory/curiosities.md", "notes/extra.md"]]
          (result, _) = run (runState @Files files (interpret readHost (C.loadEntries manifest)))
          extra = last (A.memories (must result))
      assertEqual "metadata" (["map"], ["persona"], Just 20) (A.tags extra, A.related extra, A.availableAt extra)
      assertEqual "original observed body" "notes/extra.md" (A.original extra)
  , testCase "manifest duplicates refuse before reading sources" $ do
      let manifest = C.Manifest [C.Entry "persona" "notes/imposter.md" [] [] Nothing]
          result = run (interpret rejectReads (C.loadEntries manifest))
      assertEqual "duplicate built-in source" (Left (C.InvalidCollection (A.DuplicateMemory "persona"))) result
  , testCase "lenses share identical views but refuse conflicting definitions" $ do
      let combined = must (A.combineViews [A.continuity source, A.connections source])
          (result, calls) = plan (const A.Expand) generous combined
      assertEqual "one judgment per distinct view" 4 (length (concat calls))
      assertEqual "all views reached" [] (A.notVisited (A.inspect (must result)))
      assertEqual "conflicting shared definition" (Left (A.DuplicateNode "x"))
        (A.combineViews [A.View ["x"] [node "x" []], A.View ["x"] [(node "x" []) {A.question = "a different question"}]])
  , testCase "node storage order does not change rendering or decision order" $ do
      let policy n = if null (A.children n) then A.PresentAs (A.fallback n) else A.Expand
          (first, calls) = plan policy generous diamond
          (second, reorderedCalls) = plan policy generous (diamond {A.nodes = reverse (A.nodes diamond)})
      assertEqual "frontier comes from relationships" calls reorderedCalls
      assertEqual "stable rendering" (A.rendered (must first)) (A.rendered (must second))
  , testProperty "shared chains respect caller pass and evidence bounds" $ \(NonNegative rawDepth) (NonNegative rawPasses) (NonNegative rawChars) ->
      let depth = rawDepth `mod` 20 + 1
          budget = A.Limits (rawPasses `mod` 25) (toInteger (rawChars `mod` 10000))
          keys = map (T.pack . show) [0..depth :: Int]
          v = A.View ["0", "0"] [node key (take 1 (drop (i+1) keys)) | (i,key) <- zip [0..] keys]
          (result, calls) = plan (\n -> if null (A.children n) then A.PresentAs (A.fallback n) else A.Expand) budget v
          retained = A.inspect (must result)
      in conjoin [(length calls <= A.judgmentPasses budget) === True,
                  (A.evidenceUsed retained <= A.evidenceCharacters budget) === True,
                  A.passesUsed retained === length calls,
                  length (concat calls) === Map.size (Map.fromList [(key, ()) | key <- concat calls])]
  ]

jevUnavailable :: Member (State Int) effects => Jev a -> Eff effects a
jevUnavailable (JevAskWith _) = modify (+ (1 :: Int)) >> pure (Left (JevCircuitOpen 503 987))
type Files = Map.Map Text (Either FsError Text)
readHost :: Member (State Files) effects => FsRead a -> Eff effects a
readHost (FsRead path) = Map.findWithDefault (Left (FsNotFound path)) path <$> get @Files
readHost _ = error "unexpected filesystem observation"
writeHost :: Member (State Files) effects => FsWrite a -> Eff effects a
writeHost (FsWrite path body) = modify @Files (Map.insert path (Right body)) >> pure (Right ())
writeHost (FsWriteCas path expected body) = do
  files <- get @Files
  case (Map.lookup path files, expected) of
    (Nothing, Nothing) -> modify @Files (Map.insert path (Right body)) >> pure (Right (Right ()))
    _ -> pure (Right (Left (Just "existing-file-hash")))
rejectReads :: FsRead a -> Eff '[] a
rejectReads _ = error "invalid manifest reached the filesystem"
