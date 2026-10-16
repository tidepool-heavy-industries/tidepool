{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE RankNTypes #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeOperators #-}
module Main where

import PersonaSeedTest (personaSeedTests)
import PersonaMemoryTest (personaMemoryTests)
import PersonaFileTest (personaFileTests)
import ResearcherSpecTest (researcherSpecTests)
import ReplyToolTest (replyToolTests)
import FormJevDialogueTest (formJevDialogueTests)
import FormLifecycleTest (formLifecycleTests)
import Tidepool.Test.Runner (TestTree, runTests, testCase, testGroup)
import JevPreparedTest (jevPreparedTests)
import JevRecordingTest (jevRecordingTests)
import qualified Examples.ModelCallWorkflow as ModelExample
import LookupEvidenceTest (lookupEvidenceTests)

import Prelude
import Control.Monad (unless)
import Control.Monad.Freer (Eff, interpret, run)
import Control.Monad.Freer.State (State, modify, runState)
import Data.Proxy (Proxy (..))
import Data.Text (Text)
import qualified Data.Text as Text
import Tidepool.Inspection
import Tidepool.Records (WriteOutcome(..))
import GHC.Generics (Generic)
import Tidepool.Aeson
import Tidepool.Agent.Contract
import qualified Tidepool.Reload as Reload
import Tidepool.Model
import Tidepool.Internal.ModelControl qualified as Control
import Tidepool.Effects.Core (ModelCall (..), SourceStatus(..), SourceRevision(..), SourceModule(..))

data Tools mode = Tools { echo :: mode :- Call Text Text } deriving Generic
data Reply = Reply { count :: Int, optional :: Maybe Text }
  deriving (Generic, FromJSON, ToJSON, JsonSchema)

program :: Eff '[ModelCall, State [Text]] (ModelResult Reply)
program = invokeModel (typedTurn @Reply (defaultSpec
  { specTools = Tools (presentWith (const "model-visible") (tool "Echo with a caller effect" (\text -> modify (<> [text]) >> pure text)))
  , afterTool = Just (\_ result -> modify (<> [toolResultHandle result, if toolResultValue result == String "called" then "semantic" else "missing"]) >> pure (Annotated "checked"))
  }) "Return a typed result after echoing") "hello"

handleModel :: ModelCall a -> Eff '[State [Text]] a
handleModel (ModelStartWith _) = pure (Right (toJSON
  (Control.ModelCallback "i" "c" "echo" (String "called"))))
handleModel (ModelResumeWith token call answer)
  | token == "i" && call == "c" && answer == toolDispatchReply (Right (ToolDispatchSuccess (String "called") "model-visible")) = pure (Right (toJSON
      (Control.ModelHook token "op" "echo" (String "called") "retained:1" 1 (String "called") "called")))
  | otherwise = error "callback identity/output changed"
handleModel (ModelAnnotateWith token op annotation)
  | token == "i" && op == "op" && annotation == annotationToJson (Annotated "checked") = pure (Right (toJSON
      (Control.ModelFinished (Control.ModelReceiptEnvelope token "cell" ["request"]
        (Control.ModelUsageEnvelope 2 1 42 0) (Control.ModelUsageEnvelope 2 1 42 0)
        (Control.ModelTypedOutcome (toJSON (Reply 3 Nothing)))))))
  | otherwise = error "hook identity/annotation changed"
handleModel (ModelCloseWith _) = modify (<> (["closed"] :: [Text])) >> pure (Right ())

main :: IO ()
main = runTests tests

tests :: TestTree
tests = testGroup "test-model-turn"
  [testCase "NoTools model examples preserve selectors limits and receipts" noToolsExample, testCase "source status and write outcomes expose useful details" sourceInspection, jevPreparedTests, jevRecordingTests, lookupEvidenceTests, personaSeedTests, personaMemoryTests, personaFileTests, researcherSpecTests, replyToolTests, formJevDialogueTests, formLifecycleTests, testCase "typed model callbacks hooks receipts and nullable schemas" scenario]

scenario :: IO ()
scenario = do
  unless (toolDispatchReply (Left (UnknownTool "missing")) == object
    ["status" .= ("refused" :: Text), "kind" .= ("unknown_tool" :: Text)
    ,"tool" .= ("missing" :: Text), "error" .= ("no such tool: missing" :: Text)])
    (error "unknown-tool refusal lost its typed identity")
  unless (toolDispatchReply (Left (InvalidToolInput "echo" "semantic rejection")) == object
    ["status" .= ("refused" :: Text), "kind" .= ("invalid_input" :: Text)
    ,"tool" .= ("echo" :: Text), "detail" .= ("semantic rejection" :: Text)
    ,"error" .= ("invalid input for tool echo: semantic rejection" :: Text)])
    (error "input refusal lost its typed detail")
  let (result, events) = run (runState [] (interpret handleModel program))
  unless (events == ["called", "retained:1", "semantic"]) (error "callbacks did not use caller state or semantic result")
  case modelOutcome result of
    Right (Reply 3 Nothing) -> pure ()
    _ -> error "typed result did not decode"
  unless (fmap invocationIdentity (modelReceipt result) == Just "i") (error "missing receipt")
  unless (jsonSchema (Proxy @(Maybe Int)) == object [("anyOf", Array [jsonSchema (Proxy @Int), object [("type", String "null")]])]) (error "Maybe schema omits null")
  unless (jsonSchema (Proxy @[Maybe Int]) == object [("type", String "array"), ("items", jsonSchema (Proxy @(Maybe Int)))]) (error "array optional schema drift")
  unless (jsonSchema (Proxy @Reply) == object
    [("type", String "object"), ("properties", object [("count", jsonSchema (Proxy @Int)), ("optional", jsonSchema (Proxy @(Maybe Text)))]), ("required", Array [String "count"]), ("additionalProperties", Bool False)])
    (error "record optional field omits null or becomes required")
  putStrLn "model DSL: callbacks, caller effects, retained hook, typed result, nullable schemas passed"

sourceInspection :: IO ()
sourceInspection = do
  let active = SourceRevision "captured-revision" 7 [SourceModule "Persona.Spec" "source-digest"]
      disk = SourceRevision "pending-revision" 0 []
      (rendered, _) = displayWith 4096 (Right (SourceStatus active disk) :: Either Text SourceStatus)
  mapM_ (\piece -> unless (piece `Text.isInfixOf` rendered) (error "source status omitted inspection facts"))
    ["captured-revision", "pending-revision", "Persona.Spec", "source-digest", "generation = 7", "pendingEdit = True"]
  let diff = Reload.ReloadSurfaceDiff [] [] []
      publications = [Reload.ReloadPublished active disk [] diff,
        Reload.ReloadPublishedWithWarnings active disk [] ["projection repair"] diff]
  mapM_ (\outcome -> unless ("Notebook reset; re-import modules" `Text.isInfixOf` Reload.reloadPresenter (Right outcome))
    (error "committed reload omitted notebook recovery action")) publications
  unless (Reload.reloadPresenter (Right (Reload.ReloadUnchanged active)) == "Configuration unchanged; notebook bindings preserved.")
    (error "unchanged reload lost binding preservation")
  mapM_ (\outcome -> unless (not ("Notebook reset" `Text.isInfixOf` Reload.reloadPresenter (Right outcome)))
    (error "uncommitted reload falsely claimed a reset"))
    [Reload.ReloadRejected active disk "compile failure", Reload.ReloadCancelled active,
     Reload.ReloadPublicationUnconfirmed active "reconcile"]
  let outcomes = [Written "memory.md" 2, WriteBlocked "memory.md" ["check failed"], WriteConflict "memory.md" (Just "expected-digest") Nothing]
  mapM_ (\outcome -> unless ("memory.md" `Text.isInfixOf` fst (displayWith 4096 outcome)) (error "write outcome is opaque")) outcomes
  let page = pageWithContinuation 80 (TextLeaf (Text.replicate 200 "λ")) Nothing :: DisplayPage '[]
  unless (pageHasMore page && "[truncated;" `Text.isInfixOf` text page && Text.length (text page) <= 80)
    (error "retained page omitted bounded truncation notice")


noToolsExample :: IO ()
noToolsExample = do
  let runExample label example =
        let result = run (interpret (handleText label) example)
        in case ModelExample.report result of
          (Right "bounded answer", Just (_, ["request"], usage))
            | lookup "provider_requests" usage == Just 1
            , lookup "tool_attempts" usage == Just 0 -> pure ()
          other -> error ("NoTools example lost outcome or receipt: " ++ show other)
  runExample "Luna" (ModelExample.luna "bounded input")
  runExample "Sol" (ModelExample.sol "bounded input")
  case ModelExample.report (ModelResult (Left ModelCancelled) Nothing) of
    (Left _, Nothing) -> pure ()
    other -> error ("missing receipt or failure changed: " ++ show other)
  where
    handleText :: Text -> ModelCall a -> Eff '[] a
    handleText label (ModelStartWith value) =
      case fromJSON value of
        Success (Control.ModelRequestEnvelope _ input model _ limits tools _ hook)
          | input == "bounded input"
          , fmap toJSON model == Just (object ["tag" .= label])
          , limits == Control.ModelRequestLimits (Just 1) (Just 0) Nothing Nothing
          , tools == toJSON ([] :: [Value])
          , not hook -> pure (Right (toJSON (Control.ModelFinished
              (Control.ModelReceiptEnvelope "invocation" "cell" ["request"]
                (Control.ModelUsageEnvelope 1 0 7 0) (Control.ModelUsageEnvelope 1 0 7 0)
                (Control.ModelTextOutcome "bounded answer")))))
        _ -> error "NoTools recipe changed selector, input, limits, tools or hook"
    handleText _ _ = error "NoTools recipe attempted callback, hook or close"
