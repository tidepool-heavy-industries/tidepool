{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE RankNTypes #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeOperators #-}
module Main where

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
import Tidepool.Effects.Core (ModelCall (..))

data Tools mode = Tools { echo :: mode :- Call Text Text } deriving Generic
data Reply = Reply { count :: Int, optional :: Maybe Text }
  deriving (Generic, FromJSON, ToJSON, JsonSchema)

program :: Eff '[ModelCall, State [Text]] (ModelResult Reply)
program = invokeModel (typedTurn @Reply (defaultSpec
  { specTools = Tools (tool "Echo with a caller effect" (\text -> modify (<> [text]) >> pure text))
  , afterTool = Just (\_ result -> modify (<> [toolResultHandle result]) >> pure (Annotated "checked"))
  }) "Return a typed result after echoing") "hello"

handleModel :: ModelCall a -> Eff '[State [Text]] a
handleModel (ModelStartWith _) = pure (Right (object
  ["kind" .= ("callback" :: Text), "invocation" .= ("i" :: Text), "call_id" .= ("c" :: Text), "name" .= ("echo" :: Text), "arguments" .= ("called" :: Text)]))
handleModel (ModelResumeWith token call answer)
  | token == "i" && call == "c" && answer == String "called" = pure (Right (object
      ["kind" .= ("hook" :: Text), "invocation" .= token, "operation" .= ("op" :: Text), "name" .= ("echo" :: Text), "arguments" .= ("called" :: Text), "handle" .= ("retained:1" :: Text), "ordinal" .= (1 :: Int), "output" .= ("called" :: Text)]))
  | otherwise = error "callback identity/output changed"
handleModel (ModelAnnotateWith token op annotation)
  | token == "i" && op == "op" && annotation == annotationToJson (Annotated "checked") = pure (Right (object
      ["kind" .= ("finished" :: Text), "receipt" .= object
        ["invocation_id" .= token, "parent_cell" .= ("cell" :: Text), "requests" .= (["request"] :: [Text])
        ,"counts" .= object ["requests" .= (2 :: Int), "tools" .= (1 :: Int), "reported_tokens" .= (42 :: Int), "unknown_usage_requests" .= (0 :: Int)]
        ,"outcome" .= object ["kind" .= ("typed" :: Text), "value" .= toJSON (Reply 3 Nothing)]]]))
  | otherwise = error "hook identity/annotation changed"
handleModel (ModelCloseWith _) = modify (<> (["closed"] :: [Text])) >> pure (Right ())

main :: IO ()
main = do
  let (result, events) = run (runState [] (interpret handleModel program))
  unless (events == ["called", "retained:1"]) (error "callbacks did not use caller state")
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
