{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}
module ReplyToolTest (replyToolTests) where

import Prelude
import Control.Monad.Freer (Eff, Member)
import qualified Data.Map.Strict as Map
import Data.Text (Text)
import Test.Tasty.HUnit (assertEqual, assertFailure)
import Tidepool.Test.Runner (TestTree, testCase, testGroup)
import Tidepool.Aeson.Value (Value (..))
import qualified Tidepool.Actors.Exomonad as Agent
import Tidepool.Agent.Reply.Internal (Replies, Reply (..), RequestId (..))
import Tidepool.Agent.Reply.Tool (replyToolInstaller)

-- Compiling these two public producers preserves both modes. Ordinary requests
-- retain arbitrary Haskell values; only the explicit JSON mode needs codecs.
closureRequest :: Member Replies effects => Agent.AgentRef -> Eff effects (Either Agent.RequestError (Agent.Request (Int -> Int)))
closureRequest agent = Agent.request agent ("return a function" :: Text) Agent.defaultRequestOptions
jsonRequest :: Member Replies effects => Agent.AgentRef -> Eff effects (Either Agent.RequestError (Agent.Request Int))
jsonRequest agent = Agent.requestWithReplyTool @Int agent ("return a number" :: Text) Agent.defaultRequestOptions

field :: Text -> Value -> Value
field key (Object fields) = Map.findWithDefault Null key fields
field _ _ = Null

replyToolTests :: TestTree
replyToolTests = testGroup "request-bound reply tool"
  [ testCase "compiled decoder declares a required result envelope and exact synchronous row" $
      case replyToolInstaller @Int (Reply (RequestId 19)) of
        Left problem -> assertFailure (show problem)
        Right (metadata, _) -> case field "tools" metadata of
          Array [declaration] -> do
            assertEqual "reserved endpoint" (String "reply") (field "name" declaration)
            assertEqual "result always required" (Array [String "result"])
              (field "required" (field "inputSchema" declaration))
            assertEqual "integer decoder schema" (String "integer")
              (field "type" (field "result" (field "properties" (field "inputSchema" declaration))))
            assertEqual "no general domain effects" (Array [String "ContextReadWrite", String "Reload", String "Replies"])
              (field "effectKeys" declaration)
            assertEqual "no request hook" (Array []) (field "slots" metadata)
          other -> assertFailure ("unexpected declarations: " ++ show other)
  , testCase "nullable result still requires the result envelope field" $
      case replyToolInstaller @(Maybe Int) (Reply (RequestId 20)) of
        Left problem -> assertFailure (show problem)
        Right (metadata, _) -> case field "tools" metadata of
          Array [declaration] -> assertEqual "explicit null is permitted, omission is not"
            (Array [String "result"]) (field "required" (field "inputSchema" declaration))
          other -> assertFailure (show other)
  ]
