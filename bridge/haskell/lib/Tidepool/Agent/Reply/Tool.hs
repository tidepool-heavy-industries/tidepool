{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE FlexibleInstances #-}
{-# LANGUAGE KindSignatures #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeOperators #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE UndecidableInstances #-}

-- | The request issuer compiles this decoder with the exact result dictionary
-- and reply handle. Hosting it does not grant Replies to the domain tool spec.
module Tidepool.Agent.Reply.Tool (replyToolInstaller) where

import Control.Monad.Freer (Eff)
import Data.Void (absurd)
import Data.Text (Text)
import GHC.Generics (Generic)
import Data.Proxy (Proxy (..))
import Tidepool.Aeson.FromJSON (FromJSON (..), withObject, (.:))
import Tidepool.Aeson.Schema (JsonSchema (..), objectSchema)
import Tidepool.Aeson.Value (Value)
import Tidepool.Agent.Contract
import qualified Tidepool.Agent.Reply.Internal as Replies
import Tidepool.Effects.Core (AgentTools)
import Tidepool.Inspection.Display (WorkbenchDisplay)

newtype ReplyInput result = ReplyInput { result :: result }
  deriving (Generic)
instance FromJSON result => FromJSON (ReplyInput result) where
  parseJSON = withObject "request reply" (\fields -> ReplyInput <$> fields .: "result")
instance JsonSchema result => JsonSchema (ReplyInput result) where
  jsonSchema _ = objectSchema Nothing [("result", jsonSchema (Proxy :: Proxy result), True)]

data ReplyTools result mode = ReplyTools
  { reply :: mode :- Sync (Call (ReplyInput result) ())
  } deriving (Generic)

replyToolInstaller
  :: forall result
   . (FromJSON result, JsonSchema result, WorkbenchDisplay result)
  => Replies.Reply result
  -> Either Text (Value, Eff (AgentTools ': SyncEffects '[Replies.Replies]) ())
replyToolInstaller handle =
  case prepareAgentSpec $ defaultSpec
        { specTools = ReplyTools $ presentWith (const "Reply accepted.") $
            syncTool "Return the typed result for this request. Invalid input leaves the request active; pending work must finish first." $
              \(ReplyInput value) -> absurd <$> Replies.reply handle value
        }
   of
    Left problem -> Left (renderToolCompileError problem)
    Right prepared -> Right (preparedAgentSpecMetadata prepared, installPreparedSpec prepared)
