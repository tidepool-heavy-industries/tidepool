{-# LANGUAGE DataKinds #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}
module JevPreparedTest (jevPreparedTests) where

import Prelude
import Control.Monad (replicateM_)
import Control.Monad.Freer (Eff, interpret, run)
import Control.Monad.Freer.State (State, modify, runState)
import qualified Data.Text as Text
import Test.Tasty.HUnit (assertBool, assertEqual)
import Tidepool.Test.Runner (TestTree, testCase, testGroup)
import Tidepool.Effects.Core (Jev (..), JevCallError (..))
import Tidepool.Inspection.Display (Display (..))
import Tidepool.Inspection.Tree (DisplayTree (..))
import qualified Jev.Operators as J
import qualified Jev.Recording as Recording
import Project.EvidencePattern (Evidence (..))
-- Keep the response-retaining public API example in this suite's compile gate.
import qualified Examples.JevPreparedWorkflow as Workflow
import qualified Examples.JevComposedWorkflow as Composed

-- Successful response decoding belongs to the native codec gate. These host
-- checks exercise the generated typed failure path and the pure request view;
-- they do not replace Tidepool's native JSON anchors with a test codec.
jevPreparedTests :: TestTree
jevPreparedTests = testGroup "jev-prepared"
  (testCase "request inspection preserves original payload laziness" inspectPrepared
    : testCase "explicit retained executions each issue one request" explicitExecutions
    : testCase "observation uses Jev alone and preserves the typed refusal" observeWithoutConsole
    : testCase "composed queries retain grouping and typed host failure" composedQueries
    : testCase "installed batch groups equal evidence and retains refusal" installedBatch
    : testCase "installed batch recordings replay through the same independent query" installedBatchReplay
    : testCase "empty query uses no host effect" emptyQuery
    : testCase "usage display preserves unknown versus measured zero" usageDisplay
    : [testCase ("one host call retains " ++ show failure) (retainsFailure failure)
      | failure <- failures])
  where
    failures =
      [ JevUnconfigured, JevCallCap, JevTransport "unreachable", JevTimeout
      , JevHttp 503 "unavailable", JevBodyLimit, JevMalformed "invalid body"
      , JevCircuitOpen 503 987, JevClientSetup "setup refused"
      ]

inspectPrepared :: IO ()
inspectPrepared = do
  let world = J.state (#criteria J.:= ("input belongs to the prepared request" :: Text.Text))
      packet = #route J.:= J.choice "Which path matches the criteria?"
        (J.alt #first "First path" (error "first payload demanded" :: Text.Text)
          J..| J.alt #second "Second path" (error "second payload demanded" :: Text.Text))
  case J.prepare "named-model" world packet of
    Left failure -> error (show failure)
    Right prepared -> do
      let (preview, omitted) = displayWith 8192 prepared
      assertBool "prepared preview names its owner" ("Jev.Prepared" `Text.isInfixOf` preview)
      assertBool "model remains the one prepared" ("named-model" `Text.isInfixOf` preview)
      assertBool "original input can be inspected" ("input belongs to the prepared request" `Text.isInfixOf` preview)
      assertBool "question contract can be inspected" ("Which path matches the criteria?" `Text.isInfixOf` preview)
      assertBool "preview fits the declared budget" (not omitted)

retainsFailure :: JevCallError -> IO ()
retainsFailure failure = do
  let world = J.state (#input J.:= ("synthetic typed failure control" :: Text.Text))
      packet = #ready J.:= J.noul "Is the input ready?"
  case J.prepare "named-model" world packet of
    Left issue -> error (show issue)
    Right prepared -> do
      let (result, calls) = run (runState (0 :: Int)
            (interpret (reject failure) (J.executePrepared prepared)))
      assertEqual "one production Jev effect" 1 calls
      case result of
        Left actual -> assertEqual "typed host cause is preserved" (J.Transport failure) actual
        Right _ -> error "scripted host refusal unexpectedly succeeded"

reject :: JevCallError -> Jev a -> Eff '[State Int] a
reject failure (JevAskWith _) = modify (\count -> (count :: Int) + 1) >> pure (Left failure)

explicitExecutions :: IO ()
explicitExecutions = do
  let packet captured = #route J.:= J.choice "Which path?"
        (J.alt #quick "Quick path" ((+ captured) :: Int -> Int)
          J..| J.alt #careful "Careful path" (error "unselected payload demanded" :: Int -> Int))
      prepared captured = case J.prepare "named-model" (J.state (#input J.:= ("same wire contract" :: Text.Text))) (packet captured) of
        Left issue -> error (show issue)
        Right retained -> retained
  assertEqual "captured functions are absent from the wire contract"
    (J.request (prepared 7)) (J.request (prepared 31))
  mapM_ (\count -> do
    let (_, calls) = run (runState (0 :: Int)
          (interpret (reject JevTimeout) (replicateM_ count (J.executePrepared (prepared 7)))))
    assertEqual "requests equal explicit executions, including zero" count calls)
    [0..5]

observeWithoutConsole :: IO ()
observeWithoutConsole = do
  let (result, calls) = run (runState (0 :: Int)
        (interpret (reject JevTimeout) (Workflow.observeCourier "synthetic criteria")))
  assertEqual "one request and no Console requirement" 1 calls
  case result of
    Left actual -> assertEqual "observation returns the original refusal" (J.Transport JevTimeout) actual
    Right _ -> error "scripted refusal acquired a response"

usageDisplay :: IO ()
usageDisplay = do
  let unknown = displayWith 8192 (J.Usage Nothing Nothing)
      zero = displayWith 8192 (J.Usage (Just 0) (Just 0))
  assertBool "unknown usage remains distinct in the tree" (unknown /= zero)
  case displayTree (J.Usage (Just 0) (Just 0)) of
    Constructor "Jev.Usage" fields -> assertEqual "named structured usage fields"
      ["inputTokens", "outputTokens"] (map fst fields)
    _ -> error "usage was rendered as unstructured text"


composedQueries :: IO ()
composedQueries = do
  let assess n = J.question (J.state (#evidence J.:= (n :: Int))) #ready
        (J.noul "Is the evidence ready?")
      query = traverse assess [1,2,1]
  case J.prepareQuery "named-model" query of
    Left issue -> error (show issue)
    Right ready -> do
      assertEqual "equal states group with distinct occurrences" [2,1]
        (map (length . J.groupOccurrences) (J.queryGroups ready))
      let (preview, omitted) = displayWith 32768 ready
      assertBool "group preview is structural and includes the actual model"
        ("named-model" `Text.isInfixOf` preview && not omitted)
      let (observed, calls) = run (runState (0 :: Int)
            (interpret (reject JevTimeout) (J.executeQuery ready)))
      assertEqual "refusal stops before later group" 1 calls
      assertEqual "failed attempt remains observable" 1 (length (J.observations observed))
      case J.result observed of
        Left actual -> assertEqual "typed cause crosses new surface" (J.CallFailed (J.Transport JevTimeout)) actual
        Right _ -> error "scripted refusal acquired observations"
      let (journal, _) = displayWith 32768
            (fmap (const (error "mapped payload demanded" :: Int)) observed)
      assertBool "failed evaluation has inspectable evidence" ("JevTimeout" `Text.isInfixOf` journal)

emptyQuery :: IO ()
emptyQuery = do
  let pending = Composed.reviewPending []
  case J.prepareQuery J.jevLatest pending of
    Left issue -> error (show issue)
    Right ready -> assertEqual "compiled profunctor/optic example is empty without observations" 0 (length (J.queryGroups ready))
  let (observed, calls) = run (runState (0 :: Int)
        (interpret (reject JevTimeout) (J.runQuery (pure (17 :: Int)))))
  assertEqual "pure query result" (Right 17) (J.result observed)
  assertEqual "no fabricated provider response" 0 (length (J.observations observed))
  assertEqual "no host call" 0 calls

installedBatch :: IO ()
installedBatch = do
  let query = Composed.batchQuestions "repair the E0061 call" Composed.sampleContinuations
  case J.prepareQuery J.jevLatest query of
    Left issue -> error (show issue)
    Right ready -> do
      assertEqual "independent questions share one evidence group" [2]
        (map (length . J.groupOccurrences) (J.queryGroups ready))
      let (observed, calls) = run (runState (0 :: Int)
            (interpret (reject JevTimeout) (Composed.observeBatch "repair the E0061 call" Composed.sampleContinuations)))
      assertEqual "one host attempt" 1 calls
      assertEqual "the complete failed observation survives" 1 (length (J.observations observed))
      case J.result observed of
        Left actual -> assertEqual "typed transport failure" (J.CallFailed (J.Transport JevTimeout)) actual
        Right _ -> error "refused request produced a selection"

-- Use the actual public observer and its original recordings. A typed refusal
-- exercises exact request identity without replacing native JSON intrinsics;
-- successful decoding and the selected action have a native companion test.
installedBatchReplay :: IO ()
installedBatchReplay = do
  let intent = "repair the E0061 call"
      rows = Composed.sampleContinuations
      (observed, calls) = run (runState (0 :: Int)
        (interpret (reject JevTimeout) (Composed.observeBatch intent rows)))
  assertEqual "one actual public observation" 1 calls
  archive <- either (error . show) pure (Recording.archiveFromValue
    (Recording.archiveValue (Recording.recordingArchive (J.recordings observed))))
  let changed = [row { evidenceValue = error "replay demanded a continuation" } | row <- rows]
  mapM_ (\payloads -> do
    let replayed = Composed.replayBatch J.jevLatest intent payloads archive
    case J.result replayed of
      Left actual -> assertEqual "replay preserves the original typed refusal"
        (J.CallFailed (J.Transport JevTimeout)) actual
      Right _ -> error "refused observation replayed as a successful selection"
    assertEqual "replay consumes the observer's exact original recordings"
      (J.recordings observed) (J.recordings replayed)) [rows, changed]
