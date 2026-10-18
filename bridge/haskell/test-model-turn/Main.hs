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
import qualified Persona.Spec as Persona
import LookupEvidenceTest (lookupEvidenceTests)

import Prelude
import Control.Monad (unless)
import Control.Monad.Freer (interpret, run)
import Control.Monad.Freer.Internal (Eff (..), qApp)
import Control.Monad.Freer.State (State, modify, runState)
import Data.OpenUnion (prj)
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
import Tidepool.Effects.Core (Console (..), say, ModelCall (..), SourceStatus(..), SourceRevision(..), SourceModule(..))

data Tools mode = Tools { echo :: mode :- Call Text Text } deriving Generic
data Reply = Reply { count :: Int, optional :: Maybe Text }
  deriving (Generic, FromJSON, ToJSON, JsonSchema, Show)

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
        [Control.ModelRequestObservationEnvelope "request" "deep_seek" "actual-model" (Just 7) (Just 0) Nothing Nothing 12] 23
        (Control.ModelUsageEnvelope 2 1 42 0) (Control.ModelUsageEnvelope 2 1 42 0)
        (Control.ModelTypedOutcome (toJSON (Reply 3 Nothing)))))))
  | otherwise = error "hook identity/annotation changed"
handleModel (ModelCloseWith _) = modify (<> (["closed"] :: [Text])) >> pure (Right ())

main :: IO ()
main = runTests tests

tests :: TestTree
tests = testGroup "test-model-turn"
  [testCase "NoTools model examples preserve selectors limits and receipts" noToolsExample
  , testCase "synchronous model helpers compose in the Persona notebook with Console and display" personaNotebookExample
  , testGroup "model invocation cleanup"
      [ testCase "resume failure closes the acquired callback token once" (cleanupExample ResumeFailure)
      , testCase "cleanup failure preserves the primary resume failure" (cleanupExample CloseFailure)
      , testCase "malformed continuation returns typed failure and closes once" (cleanupExample MalformedResume)
      ]
  , testCase "source status and write outcomes expose useful details" sourceInspection, jevPreparedTests, jevRecordingTests, lookupEvidenceTests, personaSeedTests, personaMemoryTests, personaFileTests, researcherSpecTests, replyToolTests, formJevDialogueTests, formLifecycleTests, testCase "typed model callbacks hooks receipts and nullable schemas" scenario]

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
  unless (fmap requestObservations (modelReceipt result) == Just
    [ModelRequestObservation "request" "deep_seek" "actual-model" (Just 7) (Just 0) Nothing Nothing 12])
    (error "public receipt lost optional counters or actual model identity")
  unless (fmap invocationDurationMs (modelReceipt result) == Just 23)
    (error "public receipt lost invocation duration")
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
  runExample "DeepSeekFlash" (ModelExample.flash "bounded input")
  runExample "DeepSeekPro" (ModelExample.pro "bounded input")
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
              (Control.ModelReceiptEnvelope "invocation" "cell" ["request"] [] 0
                (Control.ModelUsageEnvelope 1 0 7 0) (Control.ModelUsageEnvelope 1 0 7 0)
                (Control.ModelTextOutcome "bounded answer")))))
        _ -> error "NoTools recipe changed selector, input, limits, tools or hook"
    handleText _ _ = error "NoTools recipe attempted callback, hook or close"

-- This is the installed Persona notebook row, including its invocation-only
-- effects. A helper fixed to SyncEffects '[ModelCall] cannot inhabit this cell.
personaNotebook :: Eff (SyncEffects Persona.PersonaEffects)
  (ModelResult Text, ModelResult Text, (Int, Int, Int), (Int, Int, Int))
personaNotebook = do
  say "before model calls"
  flashResult <- ModelExample.synchronousFlash "persona DeepSeekFlash input"
  flashPage <- display (ModelExample.report flashResult)
  say "between model calls"
  proResult <- ModelExample.synchronousPro "persona DeepSeekPro input"
  proPage <- display (ModelExample.report proResult)
  say "after model calls"
  pure (flashResult, proResult, displayIdentity flashPage, displayIdentity proPage)

data NotebookEvent
  = ModelRequested Text Text
  | Narrated Text
  | DisplayBudgetRequested
  | DisplayPublished Text
  deriving (Eq, Show)

-- Handle the two effects this real-row program is expected to request. Reject
-- every other Persona effect instead of changing the program to a sparse row.
runPersonaNotebook :: Eff (SyncEffects Persona.PersonaEffects) a -> (a, [NotebookEvent])
runPersonaNotebook = go 1
  where
    go :: Int -> Eff (SyncEffects Persona.PersonaEffects) b -> (b, [NotebookEvent])
    go _ (Val value) = (value, [])
    go next (E effect continuation) = case prj @ModelCall effect of
      Just request -> let (answer, event) = modelStep request
        in prepend event (go next (qApp continuation answer))
      Nothing -> case prj @Console effect of
        Just (Print text) -> prepend (Narrated text) (go next (qApp continuation ()))
        Just DisplayAllowanceWith -> prepend DisplayBudgetRequested
          (go next (qApp continuation 4096))
        Just (DisplayWith (identity, text, keys, more) _)
          | identity == (0, 0, 0) && null keys && not more ->
              prepend (DisplayPublished text)
                (go (next + 1) (qApp continuation (17, 3, next)))
          | otherwise -> error "bounded report unexpectedly required display navigation"
        Just _ -> error "Persona model example requested an unexpected Console operation"
        Nothing -> error "Persona model example requested an unrelated effect"
    prepend event (answer, events) = (answer, event : events)
    modelStep :: ModelCall b -> (b, NotebookEvent)
    modelStep (ModelStartWith value) = case fromJSON value of
      Success (Control.ModelRequestEnvelope _ input model _ limits tools _ hook)
        | limits == Control.ModelRequestLimits (Just 1) (Just 0) Nothing Nothing
        , tools == toJSON ([] :: [Value])
        , not hook ->
            let label = case model of
                  Just DeepSeekFlash -> "DeepSeekFlash"
                  Just DeepSeekPro -> "DeepSeekPro"
                  _ -> error "Persona model example changed its explicit model selector"
            in (Right (toJSON (Control.ModelFinished
                  (Control.ModelReceiptEnvelope ("persona-" <> label) "persona-cell" [label <> "-request"] [] 0
                    (Control.ModelUsageEnvelope 1 0 9 0) (Control.ModelUsageEnvelope 1 0 9 0)
                    (Control.ModelTextOutcome ("answer from " <> label)))))
               , ModelRequested label input)
      _ -> error "Persona model example changed its bounded NoTools request"
    modelStep _ = error "NoTools Persona composition attempted a callback, hook or close"

personaNotebookExample :: IO ()
personaNotebookExample = do
  let ((flashResult, proResult, flashPage, proPage), events) = runPersonaNotebook personaNotebook
      require condition detail = unless condition (error detail)
  require (flashPage == (17, 3, 1) && proPage == (17, 3, 2))
    "display handles lost the identities returned by the Console owner"
  require ([ (model, input) | ModelRequested model input <- events ] ==
    [("DeepSeekFlash", "persona DeepSeekFlash input"), ("DeepSeekPro", "persona DeepSeekPro input")])
    "Persona composition changed model choice, explicit input or request multiplicity"
  mapM_ (\(label, result) -> do
    case modelOutcome result of
      Right answer -> require (answer == "answer from " <> label) "model answer changed"
      Left failure -> error (show failure)
    require (modelReceipt result == Just (ModelReceipt ("persona-" <> label)
      "persona-cell" [label <> "-request"] [] 0 (ModelUsage 1 0 9 0)))
      "Persona composition lost receipt identity or bounded usage")
    [("DeepSeekFlash", flashResult), ("DeepSeekPro", proResult)]
  case [text | DisplayPublished text <- events] of
    [flashText, proText] -> mapM_ (\(label, text) ->
      mapM_ (\piece -> require (piece `Text.isInfixOf` text)
        "display omitted the actual model answer or receipt evidence")
        ["answer from " <> label, "persona-" <> label, "persona-cell", label <> "-request",
         "provider_requests", "tool_attempts"])
      [("DeepSeekFlash", flashText), ("DeepSeekPro", proText)]
    _ -> error "Persona composition did not display both retained reports"
  require (map eventKind events ==
    ["say", "model", "allowance", "display", "say", "model", "allowance", "display", "say"])
    "Console and model effects did not follow the notebook's authored order"
  require ([text | Narrated text <- events] ==
    ["before model calls", "between model calls", "after model calls"])
    "Console text changed while raising the synchronous helpers"
  where
    eventKind (ModelRequested _ _) = "model" :: Text
    eventKind (Narrated _) = "say"
    eventKind DisplayBudgetRequested = "allowance"
    eventKind (DisplayPublished _) = "display"

data CleanupScript = ResumeFailure | CloseFailure | MalformedResume
  deriving (Eq, Show)

cleanupExample :: CleanupScript -> IO ()
cleanupExample script = do
  let (result, events) = run (runState [] (interpret cleanupHost program))
  unless (events == ["start", "called", "resume", "close:cleanup-token"])
    (error ("callback or token cleanup did not run exactly once: " ++ show events))
  unless (modelReceipt result == Nothing) (error "failed continuation fabricated a receipt")
  case (script, modelOutcome result) of
    (ResumeFailure, Left (ModelBoundary (ModelTransportFailed "resume transport failed"))) -> pure ()
    (CloseFailure, Left (ModelCleanupFailed
      (ModelBoundary (ModelTransportFailed "resume transport failed"))
      (ModelRejected "close refused"))) -> pure ()
    (MalformedResume, Left (InvalidModelResponse detail))
      | not (Text.null detail) -> pure ()
    _ -> error ("invokeModel lost the primary or cleanup failure: " ++ show result)
  where
    cleanupHost :: ModelCall a -> Eff '[State [Text]] a
    cleanupHost (ModelStartWith _) = do
      modify @[Text] (<> ["start"])
      pure (Right (toJSON (Control.ModelCallback "cleanup-token" "cleanup-call" "echo" (String "called"))))
    cleanupHost (ModelResumeWith token call answer)
      | token == "cleanup-token" && call == "cleanup-call"
      , answer == toolDispatchReply (Right (ToolDispatchSuccess (String "called") "model-visible")) = do
          modify @[Text] (<> ["resume"])
          pure (if script == MalformedResume then Right (Bool True)
            else Left (ModelTransportFailed "resume transport failed"))
      | otherwise = error "cleanup test resumed a different callback or result"
    cleanupHost (ModelCloseWith token)
      | token == "cleanup-token" = do
          modify @[Text] (<> ["close:" <> token])
          pure (if script == CloseFailure then Left (ModelRejected "close refused") else Right ())
      | otherwise = error "cleanup used a token other than the acquired invocation"
    cleanupHost (ModelAnnotateWith _ _ _) = error "failed continuation reached its after-tool hook"
