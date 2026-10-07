{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE RankNTypes #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeOperators #-}
module Main where

import FormJevDialogueTest (formJevDialogueTests)
import FormLifecycleTest (formLifecycleTests)
import Tidepool.Test.Runner (TestTree, runTests, testCase, testGroup)

import Prelude
import Control.Monad (unless)
import Control.Monad.Freer (Eff, interpret, run)
import Control.Monad.Freer.State (State, modify, runState)
import Data.Proxy (Proxy (..))
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Aeson
import Tidepool.Agent.Contract
import Tidepool.Model
import Tidepool.Internal.ModelControl qualified as Control
import Tidepool.Effects.Core (ModelCall (..))

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
  [formJevDialogueTests, formLifecycleTests, testCase "typed model callbacks hooks receipts and nullable schemas" scenario]

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
