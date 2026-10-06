{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeOperators #-}
module PreparedRuntimeSpec (agentSpec) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as Text
import Data.Void (absurd)
import GHC.Generics (Generic)
import Tidepool.Aeson.FromJSON (FromJSON)
import Tidepool.Agent.Contract
import Tidepool.Agent.Reply (Replies, currentRequest, requestReplyOf, reply)
import QuotedProvider (capture)

newtype Probe = Probe { topic :: Text }
  deriving (Generic, FromJSON, JsonSchema)

data Tools effects mode = Tools
  { notebook :: HaskellTools effects mode
  , probe :: mode :- Call Probe Text
  } deriving Generic

original :: Int
original = [capture|{quotation-input}|]

agentSpec
  :: (KnownToolEffects effects, AsyncEffects effects, Member Replies effects)
  => AgentSpec (Tools effects) effects
agentSpec = defaultSpec
  { specTools = Tools
      { notebook = haskellTools
      , probe = presentWith id $ tool (Text.pack (show original)) $ \_ -> do
          scope <- currentRequest @Text @Int
          case requestReplyOf scope of
            Nothing -> pure "no active request"
            Just destination -> absurd <$> reply destination original
      }
  }
